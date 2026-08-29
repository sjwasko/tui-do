//! The HTTP client.
//!
//! Everything that talks to a Vikunja server goes through [`Client`]. It is cheap to
//! clone (one `Arc`), so the effect runtime can hand a copy to every task without
//! wrapping it in a lock of its own.
//!
//! Three properties are enforced here rather than left to call sites:
//!
//! 1. **No URL is built by hand.** [`Client::resolve`] refuses any template that is not
//!    in [`crate::endpoints::ALL`], and the conformance test proves every entry in that
//!    list exists in `spec/vikunja.json`.
//! 2. **No page size is assumed.** Collections return a [`Pager`], which follows the
//!    server's `x-pagination-*` headers to the last page.
//! 3. **Certificate verification is never disabled.** There is no option to turn it off,
//!    because both servers sit behind publicly trusted Tailscale Serve certificates and
//!    an escape hatch would only ever be used to paper over a real problem.

use std::sync::{Arc, PoisonError, RwLock};
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE, RETRY_AFTER};
use reqwest::{Method, Url};
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::auth::{AuthKind, Credentials, Session};
use crate::endpoints;
use crate::error::{ApiError, ErrorBody, Result};
use crate::models::{
    CommentId, Label, LabelId, LabelTask, Login, Project, ProjectId, ProjectView, ServerInfo, Task,
    TaskAssignee, TaskComment, TaskId, Token, User, UserId, ViewId, DEFAULT_MAX_ITEMS_PER_PAGE,
};
use crate::pagination::Pager;
use crate::query::TaskQuery;
use crate::secret::Secret;

/// How long a single request may take before it is abandoned.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// How long to wait for the connection itself.
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The most response body the client will hold in memory, unless overridden.
///
/// Not a limit any real response approaches — a full page of fifty tasks is a few hundred
/// kilobytes — but `text()` on an unbounded body is an out-of-memory kill waiting for a
/// server that answers wrongly. Attachment downloads will stream to disk rather than come
/// through this path.
const MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;

/// One prepared request.
///
/// Kept as data rather than a `reqwest::RequestBuilder` so it can be replayed: a 401 that
/// is answered by refreshing the token has to send the identical request again, and a
/// paginator sends the same call with a different `page` each time.
#[derive(Clone)]
pub(crate) struct Call {
    method: Method,
    url: Url,
    query: Vec<(String, String)>,
    body: Option<Vec<u8>>,
    authenticated: bool,
}

impl Call {
    fn new(method: Method, url: Url) -> Self {
        Self {
            method,
            url,
            query: Vec::new(),
            body: None,
            authenticated: true,
        }
    }

    /// Mark a call as one that must not carry credentials — `/info` and `/login`.
    fn anonymous(mut self) -> Self {
        self.authenticated = false;
        self
    }

    /// Append a query parameter. Repeats are meaningful to Vikunja, so this appends.
    pub(crate) fn with_query(mut self, name: &str, value: impl Into<String>) -> Self {
        self.query.push((name.to_string(), value.into()));
        self
    }

    /// Append several query parameters.
    fn with_queries(mut self, pairs: Vec<(String, String)>) -> Self {
        self.query.extend(pairs);
        self
    }

    /// The URL, for an error message. Carries no query string, so filter and search
    /// text never reaches a log or a toast.
    pub(crate) fn url_for_error(&self) -> String {
        self.url.to_string()
    }

    /// Attach a JSON body.
    fn with_json<T: Serialize>(mut self, body: &T) -> Result<Self> {
        self.body = Some(
            serde_json::to_vec(body).map_err(|source| ApiError::Deserialize {
                url: self.url.to_string(),
                source,
            })?,
        );
        Ok(self)
    }
}

impl std::fmt::Debug for Call {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The body is redacted: `POST /login` carries a password, and a `Call` is a field
        // of `Pager`, which derives `Debug`.
        f.debug_struct("Call")
            .field("method", &self.method.as_str())
            .field("url", &self.url.as_str())
            .field("query", &self.query)
            .field(
                "body",
                &self.body.as_ref().map(|b| format!("{} bytes", b.len())),
            )
            .field("authenticated", &self.authenticated)
            .finish()
    }
}

/// Server limits, learned rather than assumed.
#[derive(Debug, Clone, Copy)]
struct Limits {
    max_items_per_page: u32,
    known: bool,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_items_per_page: DEFAULT_MAX_ITEMS_PER_PAGE,
            known: false,
        }
    }
}

#[derive(Debug)]
struct Inner {
    http: reqwest::Client,
    base: Url,
    session: RwLock<Session>,
    limits: RwLock<Limits>,
    max_response_bytes: usize,
}

/// A client for one Vikunja server.
#[derive(Debug, Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

/// Configuration for a [`Client`].
#[derive(Debug, Clone)]
pub struct ClientBuilder {
    base_url: String,
    credentials: Option<Credentials>,
    user_agent: String,
    timeout: Duration,
    connect_timeout: Duration,
    max_response_bytes: usize,
}

impl ClientBuilder {
    /// The credential to authenticate with.
    ///
    /// [`Credentials::Password`] is not used until [`Client::login`] runs; an API token
    /// is in force immediately.
    #[must_use]
    pub fn credentials(mut self, credentials: Credentials) -> Self {
        self.credentials = Some(credentials);
        self
    }

    /// Override the `User-Agent`.
    #[must_use]
    pub fn user_agent(mut self, agent: impl Into<String>) -> Self {
        self.user_agent = agent.into();
        self
    }

    /// Override the per-request timeout.
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Override how much response body will be buffered before the request is abandoned.
    ///
    /// Exists to be lowered in tests; [`MAX_RESPONSE_BYTES`] is far above any real
    /// response.
    #[must_use]
    pub fn max_response_bytes(mut self, limit: usize) -> Self {
        self.max_response_bytes = limit;
        self
    }

    /// Build the client. Performs no I/O.
    ///
    /// # Errors
    /// [`ApiError::InvalidUrl`] if the base URL cannot be parsed, or
    /// [`ApiError::Transport`] if the TLS backend cannot be initialised.
    pub fn build(self) -> Result<Client> {
        let base = normalize_base(&self.base_url)?;

        let http = reqwest::Client::builder()
            .user_agent(self.user_agent)
            .timeout(self.timeout)
            .connect_timeout(self.connect_timeout)
            // `POST /user/token/refresh` reads the refresh cookie set by `POST /login`;
            // without a jar it answers "No refresh token provided."
            .cookie_store(true)
            .build()
            .map_err(|source| ApiError::Transport {
                url: base.to_string(),
                source,
            })?;

        let session = match self.credentials {
            Some(Credentials::ApiToken(token)) => Session::ApiToken(token),
            // A password is not a credential the server accepts on a request; it becomes
            // one only after `login` exchanges it for a JWT.
            Some(Credentials::Password(_)) | None => Session::Anonymous,
        };

        Ok(Client {
            inner: Arc::new(Inner {
                http,
                base,
                session: RwLock::new(session),
                limits: RwLock::new(Limits::default()),
                max_response_bytes: self.max_response_bytes,
            }),
        })
    }
}

impl Client {
    /// Start configuring a client for `base_url`.
    ///
    /// The URL may be given with or without the `/api/v1` suffix; both
    /// `https://vikunja.example` and `https://vikunja.example/api/v1/` work.
    #[must_use]
    pub fn builder(base_url: impl Into<String>) -> ClientBuilder {
        ClientBuilder {
            base_url: base_url.into(),
            credentials: None,
            user_agent: concat!("tui-do/", env!("CARGO_PKG_VERSION")).to_string(),
            timeout: DEFAULT_TIMEOUT,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            max_response_bytes: MAX_RESPONSE_BYTES,
        }
    }

    /// The API base, including `/api/v1/`.
    #[must_use]
    pub fn base_url(&self) -> &Url {
        &self.inner.base
    }

    /// How the client is currently authenticated.
    #[must_use]
    pub fn auth_kind(&self) -> AuthKind {
        AuthKind::from(&*self.session())
    }

    /// The page size to request: the server's cap, or a conservative default until
    /// [`Client::info`] has been called.
    #[must_use]
    pub fn page_size(&self) -> u32 {
        self.limits().max_items_per_page
    }

    /// Whether the page size above came from the server rather than the default.
    #[must_use]
    pub fn page_size_is_known(&self) -> bool {
        self.limits().known
    }

    // ---- endpoints -------------------------------------------------------------

    /// `GET /info` — version, limits, enabled features.
    ///
    /// Needs no credentials, and caches `max_items_per_page` for the paginators. Call it
    /// once at startup.
    ///
    /// # Errors
    /// Any transport or status failure.
    pub async fn info(&self) -> Result<ServerInfo> {
        let call = Call::new(Method::GET, self.resolve(endpoints::INFO, &[])?).anonymous();
        let (info, _) = self.send::<ServerInfo>(call).await?;
        {
            let mut limits = self
                .inner
                .limits
                .write()
                .unwrap_or_else(PoisonError::into_inner);
            limits.max_items_per_page = info.page_cap();
            limits.known = true;
        }
        Ok(info)
    }

    /// `POST /login` — exchange a username and password for a JWT.
    ///
    /// On success the client sends that JWT on every subsequent request, and keeps the
    /// refresh cookie the server set so [`Client::refresh_token`] can renew it.
    ///
    /// # Errors
    /// [`ApiError::Unauthorized`] for a wrong password or a missing TOTP passcode
    /// (Vikunja answers 412 for the latter), [`ApiError::RateLimited`] after ten failed
    /// attempts.
    pub async fn login(&self, login: &Login) -> Result<()> {
        let call = Call::new(Method::POST, self.resolve(endpoints::LOGIN, &[])?)
            .anonymous()
            .with_json(login)?;
        let (token, _) = self.send::<Token>(call).await?;
        self.set_session(Session::Jwt(token.token));
        Ok(())
    }

    /// `POST /user/token/refresh` — renew the JWT from the refresh cookie.
    ///
    /// Called automatically when a request comes back 401 on a JWT session, so most
    /// callers never need it.
    ///
    /// # Errors
    /// [`ApiError::Unauthorized`] when the refresh cookie is missing or expired, which
    /// means the user has to log in again.
    pub async fn refresh_token(&self) -> Result<()> {
        // Two ways this is reachable when it should not be. The refresh cookie outlives
        // `logout`'s local session reset, so a caller could mint a JWT for a session the
        // user ended. And on an API-token session a successful refresh would *replace*
        // the user's configured long-lived token with a JWT, discarding a credential the
        // client cannot get back.
        if !self.session().is_refreshable() {
            return Err(ApiError::NotAuthenticated {
                action: "refreshing the session",
            });
        }
        let call = Call::new(Method::POST, self.resolve(endpoints::TOKEN_REFRESH, &[])?);
        // Deliberately `send_once`: a refresh that 401s must not trigger another refresh.
        let (token, _) = self.send_once::<Token>(&call).await?;
        self.set_session(Session::Jwt(token.token));
        Ok(())
    }

    /// `POST /user/logout` — invalidate the session server-side and forget it locally.
    ///
    /// # Errors
    /// Any transport or status failure. The local session is cleared either way.
    pub async fn logout(&self) -> Result<()> {
        let call = Call::new(Method::POST, self.resolve(endpoints::LOGOUT, &[])?);
        let result = self.send_ignoring_body(call).await;
        self.set_session(Session::Anonymous);
        result.map(|_| ())
    }

    /// `GET /user` — the authenticated user.
    ///
    /// # Errors
    /// Any transport or status failure.
    pub async fn current_user(&self) -> Result<User> {
        self.require_auth("reading the current user")?;
        let call = Call::new(Method::GET, self.resolve(endpoints::CURRENT_USER, &[])?);
        self.send::<User>(call).await.map(|(user, _)| user)
    }

    /// `GET /tasks` — a paginator over every task the user can see.
    ///
    /// # Errors
    /// [`ApiError::NotAuthenticated`] if no credential is configured.
    pub fn tasks(&self, query: &TaskQuery) -> Result<Pager<Task>> {
        self.require_auth("listing tasks")?;
        let call = Call::new(Method::GET, self.resolve(endpoints::TASKS, &[])?)
            .with_queries(query.pairs());
        Ok(Pager::new(self.clone(), call, self.page_size()))
    }

    /// Every task the user can see, across every page.
    ///
    /// # Errors
    /// Any failure from any page; a partial list is never returned.
    pub async fn all_tasks(&self, query: &TaskQuery) -> Result<Vec<Task>> {
        self.tasks(query)?.collect_all().await
    }

    /// `GET /tasks/{id}` — one task, with its comments and attachments.
    ///
    /// # Errors
    /// Any transport or status failure.
    pub async fn task(&self, id: TaskId) -> Result<Task> {
        self.require_auth("reading a task")?;
        let call = Call::new(
            Method::GET,
            self.resolve(endpoints::TASK, &[("id", &id.to_string())])?,
        );
        self.send::<Task>(call).await.map(|(task, _)| task)
    }

    /// `GET /projects/{id}/views/{view}/tasks` — the tasks of one project view.
    ///
    /// This is how the Vikunja web frontend loads a project, and the only way to get
    /// each task's manual `position`, which is stored per view.
    ///
    /// # Errors
    /// [`ApiError::NotAuthenticated`] if no credential is configured.
    pub fn view_tasks(
        &self,
        project: ProjectId,
        view: ViewId,
        query: &TaskQuery,
    ) -> Result<Pager<Task>> {
        self.require_auth("listing a project view")?;
        let call = Call::new(
            Method::GET,
            self.resolve(
                endpoints::VIEW_TASKS,
                &[("id", &project.to_string()), ("view", &view.to_string())],
            )?,
        )
        .with_queries(query.pairs());
        Ok(Pager::new(self.clone(), call, self.page_size()))
    }

    /// `GET /projects` — a paginator over the user's projects.
    ///
    /// `is_archived=true` is not a filter but an *inclusion*: the spec words it "if true,
    /// also returns all archived projects", so the default listing silently omits them.
    /// The sync engine feeds this listing to `retain_projects`, which deletes every
    /// project the listing did not name *and cascades to their tasks* — so a listing that
    /// omitted archived projects would delete the user's archived work on every pull,
    /// incremental ones included. The store carries `is_archived` and offers a filter on
    /// it; without this parameter that column could only ever be false.
    ///
    /// # Errors
    /// [`ApiError::NotAuthenticated`] if no credential is configured.
    pub fn projects(&self) -> Result<Pager<Project>> {
        self.require_auth("listing projects")?;
        let call = Call::new(Method::GET, self.resolve(endpoints::PROJECTS, &[])?)
            .with_query("is_archived", "true");
        Ok(Pager::new(self.clone(), call, self.page_size()))
    }

    /// Every project, across every page.
    ///
    /// # Errors
    /// Any failure from any page.
    pub async fn all_projects(&self) -> Result<Vec<Project>> {
        self.projects()?.collect_all().await
    }

    /// `GET /projects/{id}` — one project.
    ///
    /// # Errors
    /// Any transport or status failure.
    pub async fn project(&self, id: ProjectId) -> Result<Project> {
        self.require_auth("reading a project")?;
        let call = Call::new(
            Method::GET,
            self.resolve(endpoints::PROJECT, &[("id", &id.to_string())])?,
        );
        self.send::<Project>(call).await.map(|(project, _)| project)
    }

    /// `GET /projects/{project}/views` — the views defined on a project.
    ///
    /// # Errors
    /// Any transport or status failure.
    pub async fn project_views(&self, project: ProjectId) -> Result<Vec<ProjectView>> {
        self.require_auth("reading project views")?;
        let call = Call::new(
            Method::GET,
            self.resolve(
                endpoints::PROJECT_VIEWS,
                &[("project", &project.to_string())],
            )?,
        );
        Pager::<ProjectView>::new(self.clone(), call, self.page_size())
            .collect_all()
            .await
    }

    /// `GET /labels` — a paginator over the user's labels.
    ///
    /// # Errors
    /// [`ApiError::NotAuthenticated`] if no credential is configured.
    pub fn labels(&self) -> Result<Pager<Label>> {
        self.require_auth("listing labels")?;
        let call = Call::new(Method::GET, self.resolve(endpoints::LABELS, &[])?);
        Ok(Pager::new(self.clone(), call, self.page_size()))
    }

    /// Every label, across every page.
    ///
    /// # Errors
    /// Any failure from any page.
    pub async fn all_labels(&self) -> Result<Vec<Label>> {
        self.labels()?.collect_all().await
    }

    /// `GET /labels?s=` — every label whose title is exactly `title`.
    ///
    /// The search parameter matches *substrings*, so the exact comparison happens here: a
    /// retried `CreateLabel` uses this to find the label its lost response created, and
    /// adopting "next week" when the user asked for "next" would be worse than the
    /// duplicate it is avoiding.
    ///
    /// Case-insensitive, matching how the interface resolves a label name, because
    /// Vikunja will happily hold `Next` and `next` and the user means one thing by them.
    /// **ASCII-only** case folding, though: `Über` does not match `über`, so a retried
    /// create of a non-ASCII title creates a duplicate rather than adopting. That is the
    /// fail-safe direction — a duplicate is visible and deletable, a wrong adoption is
    /// neither — and it keeps this in step with `resolve_labels`, which folds the same
    /// way. Both would have to move together to gain Unicode folding.
    ///
    /// This exists because a replayed create is otherwise undetectable. Measured on dev
    /// 2026-08-29: creating `tui-do probe alpha` twice answered `201` twice with two
    /// different ids, and nothing in the response tells a duplicate from a first
    /// creation.
    ///
    /// # Errors
    /// [`ApiError::NotAuthenticated`] if no credential is configured, or any failure from
    /// any page.
    pub async fn labels_named(&self, title: &str) -> Result<Vec<Label>> {
        self.require_auth("searching labels")?;
        let call =
            Call::new(Method::GET, self.resolve(endpoints::LABELS, &[])?).with_query("s", title);
        let found = Pager::<Label>::new(self.clone(), call, self.page_size())
            .collect_all()
            .await?;
        Ok(found
            .into_iter()
            .filter(|label| label.title.eq_ignore_ascii_case(title))
            .collect())
    }

    // ---- writes ----------------------------------------------------------------
    //
    // Vikunja's verbs are not the REST convention: creation is `PUT` and updates are
    // `POST`. Every method below is taken from `spec/vikunja.json` rather than from what
    // the shape of the URL suggests, because guessing here is exactly how
    // `seed-from-prod.sh` ended up sending `POST` to a migration endpoint that answers
    // `405 Allow: OPTIONS, PUT`.
    //
    // The spec is not the last word either. It documents `PUT /labels/{id}` for a label
    // update, and the server answers `405` to it -- see `update_label`. Where the two
    // disagree the server wins, and only a live test can tell them apart.

    /// `PUT /projects/{id}/tasks` — create a task in a project.
    ///
    /// `project` wins over `task.project_id`, and is written into the body before
    /// sending. That is not belt and braces, it is required: Vikunja binds path
    /// parameters *first* and the JSON body *second*, so a body carrying
    /// `"project_id": 0` overwrites the id taken from the URL. The server then looks up
    /// project 0 and answers `404` with error code `3001`, "This project does not
    /// exist." — about the project you just successfully created.
    ///
    /// Returns the server's version of the task, which is what the local store should
    /// keep: it carries the assigned id, index and identifier.
    ///
    /// # Errors
    /// Any transport or status failure.
    pub async fn create_task(&self, project: ProjectId, task: &Task) -> Result<Task> {
        self.require_auth("creating a task")?;
        let body = Task {
            project_id: project,
            ..task.clone()
        };
        let call = Call::new(
            Method::PUT,
            self.resolve(endpoints::PROJECT_TASKS, &[("id", &project.to_string())])?,
        )
        .with_json(&body)?;
        self.send::<Task>(call).await.map(|(task, _)| task)
    }

    /// `POST /tasks/{id}` — update a task.
    ///
    /// Vikunja replaces the task from the body, so send a whole task that was read,
    /// mutated and passed back — not a sparse one. A missing field is a cleared field.
    ///
    /// Two consequences worth stating plainly:
    ///
    /// - **Assignees are part of the body**, and sending an empty list clears them.
    ///   Vikunja's list endpoints do populate assignees — verified against dev — so a
    ///   listed task can be passed straight back. A task *assembled* from partial data
    ///   cannot: it will unassign everyone.
    /// - **Labels are not.** They are attached and detached through
    ///   [`Client::add_label_to_task`] and [`Client::remove_label_from_task`], and the
    ///   `labels` field on the body is ignored.
    ///
    /// # Errors
    /// Any transport or status failure.
    pub async fn update_task(&self, task: &Task) -> Result<Task> {
        self.require_auth("updating a task")?;
        let call = Call::new(
            Method::POST,
            self.resolve(endpoints::TASK, &[("id", &task.id.to_string())])?,
        )
        .with_json(task)?;
        self.send::<Task>(call).await.map(|(task, _)| task)
    }

    /// `DELETE /tasks/{id}`.
    ///
    /// # Errors
    /// Any transport or status failure.
    pub async fn delete_task(&self, id: TaskId) -> Result<()> {
        self.require_auth("deleting a task")?;
        let call = Call::new(
            Method::DELETE,
            self.resolve(endpoints::TASK, &[("id", &id.to_string())])?,
        );
        self.send_ignoring_body(call).await.map(|_| ())
    }

    /// `PUT /projects` — create a project.
    ///
    /// # Errors
    /// Any transport or status failure.
    pub async fn create_project(&self, project: &Project) -> Result<Project> {
        self.require_auth("creating a project")?;
        let call =
            Call::new(Method::PUT, self.resolve(endpoints::PROJECTS, &[])?).with_json(project)?;
        self.send::<Project>(call).await.map(|(project, _)| project)
    }

    /// `POST /projects/{id}` — update a project.
    ///
    /// # Errors
    /// Any transport or status failure.
    pub async fn update_project(&self, project: &Project) -> Result<Project> {
        self.require_auth("updating a project")?;
        let call = Call::new(
            Method::POST,
            self.resolve(endpoints::PROJECT, &[("id", &project.id.to_string())])?,
        )
        .with_json(project)?;
        self.send::<Project>(call).await.map(|(project, _)| project)
    }

    /// `DELETE /projects/{id}` — delete a project and everything in it.
    ///
    /// # Errors
    /// Any transport or status failure.
    pub async fn delete_project(&self, id: ProjectId) -> Result<()> {
        self.require_auth("deleting a project")?;
        let call = Call::new(
            Method::DELETE,
            self.resolve(endpoints::PROJECT, &[("id", &id.to_string())])?,
        );
        self.send_ignoring_body(call).await.map(|_| ())
    }

    /// `PUT /labels` — create a label.
    ///
    /// # Errors
    /// Any transport or status failure.
    pub async fn create_label(&self, label: &Label) -> Result<Label> {
        self.require_auth("creating a label")?;
        let call =
            Call::new(Method::PUT, self.resolve(endpoints::LABELS, &[])?).with_json(label)?;
        self.send::<Label>(call).await.map(|(label, _)| label)
    }

    /// `POST /labels/{id}` — update a label.
    ///
    /// The spec says `put`. The server answers `405 Method Not Allowed` to a `PUT` here
    /// and `OPTIONS /labels/{id}` replies `Allow: OPTIONS, DELETE, GET, POST` — measured
    /// on dev 2026-08-29, and the fourth time the spec has lost to the server on a verb
    /// or a body. Nothing called this method, so the `405` was never seen.
    ///
    /// The whole label goes in the body, not the fields being changed: a body carrying
    /// only `title` cleared `hex_color` to `""`, exactly as a partial task body clears
    /// what it omits.
    ///
    /// Takes the label rather than an id and a patch **because the body's `id` beats the
    /// path**: `POST /labels/12` carrying `"id": 13` updated label 13 and left 12
    /// untouched, answering with 13. One argument means the two cannot disagree.
    ///
    /// # Errors
    /// Any transport or status failure. `404` with Vikunja code `8002` when the label is
    /// gone, which is what a replayed rename after a delete answers.
    pub async fn update_label(&self, label: &Label) -> Result<Label> {
        self.require_auth("updating a label")?;
        let call = Call::new(
            Method::POST,
            self.resolve(endpoints::LABEL, &[("id", &label.id.to_string())])?,
        )
        .with_json(label)?;
        self.send::<Label>(call).await.map(|(label, _)| label)
    }

    /// `DELETE /labels/{id}`.
    ///
    /// # Errors
    /// Any transport or status failure.
    pub async fn delete_label(&self, id: LabelId) -> Result<()> {
        self.require_auth("deleting a label")?;
        let call = Call::new(
            Method::DELETE,
            self.resolve(endpoints::LABEL, &[("id", &id.to_string())])?,
        );
        self.send_ignoring_body(call).await.map(|_| ())
    }

    /// `GET /tasks/{task}/labels` — the labels attached to a task.
    ///
    /// # Errors
    /// Any transport or status failure.
    pub async fn task_labels(&self, task: TaskId) -> Result<Vec<Label>> {
        self.require_auth("reading task labels")?;
        let call = Call::new(
            Method::GET,
            self.resolve(endpoints::TASK_LABELS, &[("task", &task.to_string())])?,
        );
        Pager::<Label>::new(self.clone(), call, self.page_size())
            .collect_all()
            .await
    }

    /// `PUT /tasks/{task}/labels` — attach an existing label to a task.
    ///
    /// # Errors
    /// Any transport or status failure.
    pub async fn add_label_to_task(&self, task: TaskId, label: LabelId) -> Result<()> {
        self.require_auth("labelling a task")?;
        let call = Call::new(
            Method::PUT,
            self.resolve(endpoints::TASK_LABELS, &[("task", &task.to_string())])?,
        )
        .with_json(&LabelTask { label_id: label })?;
        self.send_ignoring_body(call).await.map(|_| ())
    }

    /// `DELETE /tasks/{task}/labels/{label}` — detach a label from a task.
    ///
    /// # Errors
    /// Any transport or status failure.
    pub async fn remove_label_from_task(&self, task: TaskId, label: LabelId) -> Result<()> {
        self.require_auth("unlabelling a task")?;
        let call = Call::new(
            Method::DELETE,
            self.resolve(
                endpoints::TASK_LABEL,
                &[("task", &task.to_string()), ("label", &label.to_string())],
            )?,
        );
        self.send_ignoring_body(call).await.map(|_| ())
    }

    /// `PUT /tasks/{taskID}/assignees` — assign a user to a task.
    ///
    /// # Errors
    /// Any transport or status failure.
    pub async fn assign_user(&self, task: TaskId, user: UserId) -> Result<()> {
        self.require_auth("assigning a task")?;
        let call = Call::new(
            Method::PUT,
            self.resolve(endpoints::TASK_ASSIGNEES, &[("taskID", &task.to_string())])?,
        )
        .with_json(&TaskAssignee { user_id: user })?;
        self.send_ignoring_body(call).await.map(|_| ())
    }

    /// `DELETE /tasks/{taskID}/assignees/{userID}` — unassign a user.
    ///
    /// # Errors
    /// Any transport or status failure.
    pub async fn unassign_user(&self, task: TaskId, user: UserId) -> Result<()> {
        self.require_auth("unassigning a task")?;
        let call = Call::new(
            Method::DELETE,
            self.resolve(
                endpoints::TASK_ASSIGNEE,
                &[("taskID", &task.to_string()), ("userID", &user.to_string())],
            )?,
        );
        self.send_ignoring_body(call).await.map(|_| ())
    }

    /// `GET /tasks/{taskID}/comments` — the comments on a task.
    ///
    /// # Errors
    /// Any transport or status failure.
    pub async fn task_comments(&self, task: TaskId) -> Result<Vec<TaskComment>> {
        self.require_auth("reading comments")?;
        let call = Call::new(
            Method::GET,
            self.resolve(endpoints::TASK_COMMENTS, &[("taskID", &task.to_string())])?,
        );
        Pager::<TaskComment>::new(self.clone(), call, self.page_size())
            .collect_all()
            .await
    }

    /// `PUT /tasks/{taskID}/comments` — post a comment.
    ///
    /// # Errors
    /// Any transport or status failure.
    pub async fn create_comment(
        &self,
        task: TaskId,
        text: impl Into<String>,
    ) -> Result<TaskComment> {
        self.require_auth("commenting on a task")?;
        let body = TaskComment {
            comment: text.into(),
            ..TaskComment::default()
        };
        let call = Call::new(
            Method::PUT,
            self.resolve(endpoints::TASK_COMMENTS, &[("taskID", &task.to_string())])?,
        )
        .with_json(&body)?;
        self.send::<TaskComment>(call)
            .await
            .map(|(comment, _)| comment)
    }

    /// `POST /tasks/{taskID}/comments/{commentID}` — edit a comment.
    ///
    /// The spec declares no request body for this operation, which cannot be right: there
    /// is no other way to say what the comment should now be. Treated as the same kind of
    /// upstream spec bug as the `PUT`-versus-`POST` on the migration endpoint, and the
    /// body is sent. The live test is what confirms it.
    ///
    /// # Errors
    /// Any transport or status failure.
    pub async fn update_comment(&self, task: TaskId, comment: &TaskComment) -> Result<TaskComment> {
        self.require_auth("editing a comment")?;
        let call = Call::new(
            Method::POST,
            self.resolve(
                endpoints::TASK_COMMENT,
                &[
                    ("taskID", &task.to_string()),
                    ("commentID", &comment.id.to_string()),
                ],
            )?,
        )
        .with_json(comment)?;
        self.send::<TaskComment>(call)
            .await
            .map(|(comment, _)| comment)
    }

    /// `DELETE /tasks/{taskID}/comments/{commentID}`.
    ///
    /// # Errors
    /// Any transport or status failure.
    pub async fn delete_comment(&self, task: TaskId, comment: CommentId) -> Result<()> {
        self.require_auth("deleting a comment")?;
        let call = Call::new(
            Method::DELETE,
            self.resolve(
                endpoints::TASK_COMMENT,
                &[
                    ("taskID", &task.to_string()),
                    ("commentID", &comment.to_string()),
                ],
            )?,
        );
        self.send_ignoring_body(call).await.map(|_| ())
    }

    // ---- request machinery ------------------------------------------------------

    /// Turn a path template from [`endpoints`] into a URL.
    ///
    /// Refuses templates that are not in [`endpoints::ALL`]: the conformance test proves
    /// that list matches the spec, and this makes that proof binding at runtime too.
    fn resolve(&self, template: &'static str, params: &[(&str, &str)]) -> Result<Url> {
        if !endpoints::ALL.contains(&template) {
            return Err(ApiError::UnknownEndpoint {
                template: template.to_string(),
            });
        }

        let mut path = template.to_string();
        for (name, value) in params {
            // Every path parameter Vikunja takes is a number or a short enum word, so
            // allowing exactly that is both sufficient and airtight. An allowlist cannot
            // be outflanked by the next encoding trick, and it rejects `..` as readily
            // as `/`.
            if value.is_empty()
                || !value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            {
                return Err(ApiError::InvalidUrl {
                    url: template.to_string(),
                    reason: format!("path parameter {name} is not a plain value: {value:?}"),
                });
            }
            path = path.replace(&format!("{{{name}}}"), value);
        }

        if path.contains('{') {
            return Err(ApiError::InvalidUrl {
                url: template.to_string(),
                reason: format!("unfilled path parameter in {path}"),
            });
        }

        self.inner
            .base
            .join(path.trim_start_matches('/'))
            .map_err(|e| ApiError::InvalidUrl {
                url: format!("{}{path}", self.inner.base),
                reason: e.to_string(),
            })
    }

    /// Send a call, retrying once through a token refresh if it comes back 401.
    pub(crate) async fn send<T: DeserializeOwned>(&self, call: Call) -> Result<(T, HeaderMap)> {
        self.with_refresh(&call, |client, call| {
            Box::pin(async move { client.send_once::<T>(call).await })
        })
        .await
    }

    /// Run `attempt`, and on a 401 from a refreshable session refresh the token and run
    /// it once more.
    ///
    /// Only 401 takes this path. A 403 is a permission denial that a new token will not
    /// change, and refreshing on it would spend the rate-limited refresh endpoint on
    /// every forbidden resource a sync pass touches.
    async fn with_refresh<'a, T, F>(&'a self, call: &'a Call, attempt: F) -> Result<T>
    where
        F: for<'b> Fn(
            &'b Client,
            &'b Call,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<T>> + Send + 'b>,
        >,
    {
        match attempt(self, call).await {
            Err(ApiError::Unauthorized { code, message })
                if call.authenticated && self.session().is_refreshable() =>
            {
                // A JWT that expired mid-session is routine, not an error worth showing.
                // If the refresh also fails, report the original rejection: "your token
                // expired" is more useful than "the refresh cookie is gone too".
                //
                // Two requests racing here will each refresh, which is harmless -- both
                // tokens are valid. Serialising it would need an async mutex, and so a
                // tokio dependency, for no benefit.
                match self.refresh_token().await {
                    Ok(()) => attempt(self, call).await,
                    Err(_) => Err(ApiError::Unauthorized { code, message }),
                }
            }
            other => other,
        }
    }

    /// Send a call exactly once and deserialize the response body.
    async fn send_once<T: DeserializeOwned>(&self, call: &Call) -> Result<(T, HeaderMap)> {
        let (status, headers, body) = self.dispatch(call).await?;
        if !status.is_success() {
            return Err(classify(status.as_u16(), &headers, &body));
        }
        let parsed = serde_json::from_str::<T>(&body).map_err(|source| ApiError::Deserialize {
            url: call.url.to_string(),
            source,
        })?;
        Ok((parsed, headers))
    }

    /// Send a call and check the status, discarding the body.
    ///
    /// For endpoints whose success response is a bare confirmation message. Shares
    /// [`Client::with_refresh`] with [`Client::send`]: every delete and every
    /// attach/detach goes through here, and they must survive an expired JWT exactly as
    /// well as an edit does. They did not, until a review noticed that editing a task
    /// recovered from expiry while deleting the same task failed.
    async fn send_ignoring_body(&self, call: Call) -> Result<HeaderMap> {
        self.with_refresh(&call, |client, call| {
            Box::pin(async move {
                let (status, headers, body) = client.dispatch(call).await?;
                if !status.is_success() {
                    return Err(classify(status.as_u16(), &headers, &body));
                }
                Ok(headers)
            })
        })
        .await
    }

    /// Perform the HTTP request. No status interpretation happens here.
    async fn dispatch(&self, call: &Call) -> Result<(reqwest::StatusCode, HeaderMap, String)> {
        let mut request = self
            .inner
            .http
            .request(call.method.clone(), call.url.clone());

        if !call.query.is_empty() {
            request = request.query(&call.query);
        }
        if let Some(body) = &call.body {
            request = request
                .header(CONTENT_TYPE, "application/json")
                .body(body.clone());
        }
        if call.authenticated {
            if let Some(bearer) = self.bearer_header() {
                request = request.header(AUTHORIZATION, bearer);
            }
        }

        let response = request.send().await.map_err(|source| ApiError::Transport {
            url: call.url.to_string(),
            source,
        })?;

        let status = response.status();
        let headers = response.headers().clone();
        let body = read_bounded(response, &call.url, self.inner.max_response_bytes).await?;

        tracing::trace!(method = %call.method, url = %call.url, %status, bytes = body.len(), "vikunja request");
        Ok((status, headers, body))
    }

    /// The `Authorization` header value, marked sensitive so it is redacted by anything
    /// that respects the flag.
    fn bearer_header(&self) -> Option<HeaderValue> {
        let session = self.session();
        let token = session.bearer()?;
        let mut value = HeaderValue::from_str(&format!("Bearer {}", token.expose())).ok()?;
        value.set_sensitive(true);
        Some(value)
    }

    /// Fail before sending when a request needs credentials the client does not have.
    fn require_auth(&self, action: &'static str) -> Result<()> {
        if matches!(&*self.session(), Session::Anonymous) {
            return Err(ApiError::NotAuthenticated { action });
        }
        Ok(())
    }

    fn session(&self) -> std::sync::RwLockReadGuard<'_, Session> {
        self.inner
            .session
            .read()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn set_session(&self, session: Session) {
        *self
            .inner
            .session
            .write()
            .unwrap_or_else(PoisonError::into_inner) = session;
    }

    fn limits(&self) -> Limits {
        *self
            .inner
            .limits
            .read()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Replace the credential in use, e.g. after the user pastes a new API token.
    pub fn set_api_token(&self, token: impl Into<Secret>) {
        self.set_session(Session::ApiToken(token.into()));
    }
}

/// Read a response body with a ceiling on how much is buffered.
async fn read_bounded(mut response: reqwest::Response, url: &Url, limit: usize) -> Result<String> {
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|source| ApiError::Transport {
            url: url.to_string(),
            source,
        })?
    {
        if body.len() + chunk.len() > limit {
            return Err(ApiError::ResponseTooLarge {
                url: url.to_string(),
                limit,
            });
        }
        body.extend_from_slice(&chunk);
    }
    // Vikunja answers in UTF-8 JSON; anything else is already a failure, and lossy
    // decoding keeps that failure legible instead of turning it into a transport error.
    Ok(String::from_utf8_lossy(&body).into_owned())
}

/// Interpret a failing response.
fn classify(status: u16, headers: &HeaderMap, body: &str) -> ApiError {
    ApiError::from_status(status, &ErrorBody::parse(body), retry_after(headers))
}

/// How long the server asked us to wait, from either header it might use.
///
/// Vikunja's rate limiter sends `x-ratelimit-reset` as an absolute unix timestamp; a
/// proxy in front of it may send a plain `Retry-After` in seconds.
fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    if let Some(seconds) = headers
        .get(RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
    {
        return Some(Duration::from_secs(seconds));
    }

    let reset = headers
        .get("x-ratelimit-reset")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<i64>().ok())?;

    // Vikunja sends an absolute unix timestamp here -- observed directly against the dev
    // instance, which answered `x-ratelimit-reset: 1787583384` on 2026-08-24. The spec
    // documents neither the header nor its units, so a value too small to be a plausible
    // timestamp is read as seconds-remaining instead. Guessing wrong in that direction
    // would clamp to zero and turn an honoured back-off into a retry storm against the
    // endpoint that just rate-limited us.
    const EARLIEST_PLAUSIBLE_TIMESTAMP: i64 = 1_000_000_000; // 2001-09-09
    let seconds = if reset < EARLIEST_PLAUSIBLE_TIMESTAMP {
        reset.max(0)
    } else {
        let now = chrono::Utc::now().timestamp();
        // A window that has already elapsed means "retry now", which is different from
        // "the server gave no advice" -- so clamp rather than letting the conversion fail.
        reset.saturating_sub(now).max(0)
    };
    u64::try_from(seconds).ok().map(Duration::from_secs)
}

/// Normalise a configured base URL into the API root.
///
/// Accepts what a user would reasonably paste — the server root, the API root, with or
/// without a trailing slash — and produces a URL ending in `/api/v1/` so that joining a
/// relative path works.
fn normalize_base(raw: &str) -> Result<Url> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ApiError::InvalidUrl {
            url: raw.to_string(),
            reason: "no server URL configured".to_string(),
        });
    }

    let mut url = Url::parse(trimmed).map_err(|e| ApiError::InvalidUrl {
        url: trimmed.to_string(),
        reason: e.to_string(),
    })?;

    if !matches!(url.scheme(), "http" | "https") {
        return Err(ApiError::InvalidUrl {
            url: trimmed.to_string(),
            reason: format!("unsupported scheme {}", url.scheme()),
        });
    }
    if url.host().is_none() {
        return Err(ApiError::InvalidUrl {
            url: trimmed.to_string(),
            reason: "no host".to_string(),
        });
    }

    // The bearer token goes on every request, so plaintext is worth saying out loud.
    // Loopback is exempt: that is the test server, not a network hop.
    if url.scheme() == "http" && !matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "::1"))
    {
        tracing::warn!(
            %url,
            "connecting over plaintext HTTP: the API token will cross the network in the clear"
        );
    }

    let path = url.path().trim_end_matches('/').to_string();
    let path = if path.ends_with("/api/v1") {
        path
    } else {
        format!("{path}/api/v1")
    };
    url.set_path(&format!("{path}/"));
    url.set_query(None);
    url.set_fragment(None);
    // Credentials in the URL would end up in every error message and trace line, which
    // is exactly what `Secret` exists to prevent. Vikunja does not use HTTP basic auth
    // outside CalDAV, so there is nothing to lose by dropping them.
    let _ = url.set_username("");
    let _ = url.set_password(None);
    Ok(url)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn client(base: &str) -> Client {
        Client::builder(base).build().expect("valid base url")
    }

    #[test]
    fn the_api_prefix_is_added_once_and_only_once() {
        for input in [
            "https://vikunja.example",
            "https://vikunja.example/",
            "https://vikunja.example/api/v1",
            "https://vikunja.example/api/v1/",
            "  https://vikunja.example  ",
        ] {
            assert_eq!(
                normalize_base(input).unwrap().as_str(),
                "https://vikunja.example/api/v1/",
                "input: {input:?}"
            );
        }
    }

    #[test]
    fn a_subpath_deployment_keeps_its_prefix() {
        assert_eq!(
            normalize_base("https://example.test/vikunja")
                .unwrap()
                .as_str(),
            "https://example.test/vikunja/api/v1/"
        );
    }

    #[test]
    fn the_dev_server_url_resolves_as_expected() {
        let client = client("https://dev-box.example.net:8443");
        assert_eq!(
            client.resolve(endpoints::TASKS, &[]).unwrap().as_str(),
            "https://dev-box.example.net:8443/api/v1/tasks"
        );
        assert_eq!(
            client
                .resolve(endpoints::TASK, &[("id", "42")])
                .unwrap()
                .as_str(),
            "https://dev-box.example.net:8443/api/v1/tasks/42"
        );
        assert_eq!(
            client
                .resolve(endpoints::VIEW_TASKS, &[("id", "3"), ("view", "7")])
                .unwrap()
                .as_str(),
            "https://dev-box.example.net:8443/api/v1/projects/3/views/7/tasks"
        );
    }

    #[test]
    fn a_url_that_is_not_a_url_is_rejected() {
        assert!(matches!(
            normalize_base("not a url"),
            Err(ApiError::InvalidUrl { .. })
        ));
        assert!(matches!(
            normalize_base("ftp://vikunja.example"),
            Err(ApiError::InvalidUrl { .. })
        ));
        assert!(matches!(
            normalize_base(""),
            Err(ApiError::InvalidUrl { .. })
        ));
    }

    #[test]
    fn an_endpoint_outside_the_spec_cannot_be_called() {
        // The runtime half of rule 3. cria's `/tasks/all` would stop here even if someone
        // formatted the URL by hand rather than going through `endpoints`.
        let client = client("https://vikunja.example");
        assert!(matches!(
            client.resolve("/tasks/all", &[]),
            Err(ApiError::UnknownEndpoint { .. })
        ));
    }

    #[test]
    fn an_unfilled_path_parameter_is_an_error_not_a_literal_brace() {
        let client = client("https://vikunja.example");
        assert!(matches!(
            client.resolve(endpoints::TASK, &[]),
            Err(ApiError::InvalidUrl { .. })
        ));
    }

    #[test]
    fn a_path_parameter_cannot_smuggle_url_structure() {
        let client = client("https://vikunja.example");
        for hostile in [
            "1/../projects",
            "..",
            "1%2F..",
            "1?filter=x",
            "1#frag",
            "1 2",
            "",
            "1/",
        ] {
            assert!(
                matches!(
                    client.resolve(endpoints::TASK, &[("id", hostile)]),
                    Err(ApiError::InvalidUrl { .. })
                ),
                "accepted {hostile:?} as a path parameter"
            );
        }
    }

    #[test]
    fn ordinary_path_parameters_still_pass() {
        // The allowlist has to admit what the API actually uses: numeric ids and the
        // relation kinds, which are lowercase words.
        let client = client("https://vikunja.example");
        assert!(client.resolve(endpoints::TASK, &[("id", "3876")]).is_ok());
        assert!(client
            .resolve(
                endpoints::TASK_RELATION,
                &[
                    ("taskID", "1"),
                    ("relationKind", "subtask"),
                    ("otherTaskID", "2"),
                ],
            )
            .is_ok());
    }

    #[test]
    fn requests_needing_auth_fail_before_they_are_sent() {
        let client = client("https://vikunja.example");
        assert_eq!(client.auth_kind(), AuthKind::Anonymous);
        assert!(matches!(
            client.tasks(&TaskQuery::new()),
            Err(ApiError::NotAuthenticated { .. })
        ));
    }

    #[test]
    fn an_api_token_is_in_force_immediately_but_a_password_is_not() {
        let with_token = Client::builder("https://vikunja.example")
            .credentials(Credentials::api_token("tk_x"))
            .build()
            .unwrap();
        assert_eq!(with_token.auth_kind(), AuthKind::ApiToken);

        let with_password = Client::builder("https://vikunja.example")
            .credentials(Credentials::password("swasko", "hunter2"))
            .build()
            .unwrap();
        assert_eq!(with_password.auth_kind(), AuthKind::Anonymous);
    }

    #[test]
    fn the_page_size_is_conservative_until_info_says_otherwise() {
        let client = client("https://vikunja.example");
        assert_eq!(client.page_size(), DEFAULT_MAX_ITEMS_PER_PAGE);
        assert!(!client.page_size_is_known());
    }

    #[test]
    fn the_authorization_header_is_marked_sensitive() {
        let client = client("https://vikunja.example");
        client.set_api_token("tk_secret");
        let header = client.bearer_header().unwrap();
        assert!(header.is_sensitive());
    }

    #[test]
    fn a_call_does_not_print_its_body() {
        let client = client("https://vikunja.example");
        let call = Call::new(Method::POST, client.resolve(endpoints::LOGIN, &[]).unwrap())
            .with_json(&Login::new("swasko", "hunter2"))
            .unwrap();
        let rendered = format!("{call:?}");
        assert!(!rendered.contains("hunter2"));
        assert!(rendered.contains("bytes"));
    }

    #[test]
    fn retry_after_prefers_the_explicit_header() {
        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, HeaderValue::from_static("30"));
        assert_eq!(retry_after(&headers), Some(Duration::from_secs(30)));
    }

    #[test]
    fn a_rate_limit_reset_in_the_past_is_a_zero_wait_not_an_underflow() {
        // A real timestamp, long past: "retry now", not a negative duration and not
        // "the server gave no advice".
        let mut headers = HeaderMap::new();
        headers.insert("x-ratelimit-reset", HeaderValue::from_static("1600000000"));
        assert_eq!(retry_after(&headers), Some(Duration::ZERO));
    }

    #[test]
    fn a_rate_limit_reset_reads_as_a_timestamp_the_way_vikunja_sends_it() {
        // Observed against the dev instance on 2026-08-24. The value is far in the
        // future relative to `now` only while that date is in the past, so assert the
        // shape rather than an exact figure: a plausible timestamp must not be read as
        // a wait of 1.7 billion seconds.
        let mut headers = HeaderMap::new();
        headers.insert("x-ratelimit-reset", HeaderValue::from_static("1787583384"));
        let wait = retry_after(&headers).expect("a wait");
        assert!(
            wait < Duration::from_secs(86_400),
            "an absolute timestamp was read as seconds-remaining: {wait:?}"
        );
    }

    #[test]
    fn a_small_rate_limit_reset_is_read_as_seconds_remaining() {
        // The spec documents neither the header nor its units. If a proxy or a future
        // Vikunja sends a duration, reading it as a timestamp would clamp to zero and
        // turn the back-off into a retry storm.
        let mut headers = HeaderMap::new();
        headers.insert("x-ratelimit-reset", HeaderValue::from_static("30"));
        assert_eq!(retry_after(&headers), Some(Duration::from_secs(30)));
    }

    #[test]
    fn no_rate_limit_headers_means_no_advice() {
        assert_eq!(retry_after(&HeaderMap::new()), None);
    }
}
