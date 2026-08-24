//! Pagination as the server describes it, never as the client assumes it.
//!
//! This module exists because of one bug. cria asks for `per_page=10000`, the server
//! silently caps it at its configured `max_items_per_page` (50 on both our instances),
//! and cria then reads the 50 tasks it got back as the complete list. Against the dev
//! instance's 3,876 tasks that is 98.7% of the data missing, with nothing in the UI to
//! suggest it.
//!
//! The rule here is that the server's own headers are the only authority:
//!
//! - `x-pagination-total-pages` — how many pages exist for this request;
//! - `x-pagination-result-count` — how many items came back in this one.
//!
//! Nothing infers a total from a requested page size, and a collection is only complete
//! when the server says the last page has been read.

use std::marker::PhantomData;

use reqwest::header::HeaderMap;
use serde::de::DeserializeOwned;

use crate::client::{Call, Client};
use crate::error::{ApiError, Result};

/// Header carrying the number of pages available for a request.
pub const TOTAL_PAGES_HEADER: &str = "x-pagination-total-pages";

/// Header carrying the number of items returned by a request.
pub const RESULT_COUNT_HEADER: &str = "x-pagination-result-count";

/// A hard stop on how many pages one collection may walk.
///
/// At the servers' 50-per-page cap this is half a million items — far past any real
/// Vikunja, and near enough to unreachable that reaching it means the server is ignoring
/// the `page` parameter rather than that the user has a lot of tasks. Hitting it is an
/// [`ApiError::TooManyPages`], never a quiet stop: a collection that ends early and
/// reports success is the failure this module exists to prevent.
const MAX_PAGES: u32 = 10_000;

/// Where a page sits in the collection it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PageInfo {
    /// 1-based index of this page.
    pub page: u32,

    /// The `per_page` that was requested. The server may have capped it; compare against
    /// `result_count` to find out.
    pub requested_per_page: u32,

    /// `x-pagination-total-pages`, absent when the endpoint does not paginate.
    pub total_pages: Option<u32>,

    /// `x-pagination-result-count`, absent when the endpoint does not paginate.
    pub result_count: Option<u32>,

    /// How many items the server actually serves per page.
    ///
    /// Equal to `requested_per_page` until a first page proves otherwise. When an
    /// endpoint sends no pagination headers, this is the only way to tell a genuinely
    /// final short page from page one of a request the server quietly capped — the
    /// difference between a complete collection and a truncated one.
    pub effective_per_page: u32,
}

impl PageInfo {
    /// Read the pagination headers off a response.
    pub(crate) fn from_headers(
        headers: &HeaderMap,
        page: u32,
        requested_per_page: u32,
        effective_per_page: u32,
    ) -> Self {
        Self {
            page,
            requested_per_page,
            total_pages: header_u32(headers, TOTAL_PAGES_HEADER),
            result_count: header_u32(headers, RESULT_COUNT_HEADER),
            effective_per_page,
        }
    }

    /// Whether another page should be requested after this one.
    ///
    /// Prefers the server's page count. Without one, a page is the last when it is
    /// shorter than what the server has been serving — `effective_per_page`, learned
    /// from the first page rather than assumed from what was asked for. That distinction
    /// is the whole point: comparing against the *requested* size makes page one of a
    /// silently capped request look like the final page, which is cria's truncation bug
    /// wearing a different hat.
    #[must_use]
    pub fn has_more(self, items_on_this_page: usize) -> bool {
        match self.total_pages {
            Some(total) => self.page < total,
            None => items_on_this_page > 0 && items_on_this_page as u32 >= self.effective_per_page,
        }
    }

    /// Whether the server returned fewer items than asked for while more pages remain,
    /// which means it capped `per_page`.
    ///
    /// Exactly the condition cria never checked.
    #[must_use]
    pub fn was_capped(self, items_on_this_page: usize) -> bool {
        (items_on_this_page as u32) < self.requested_per_page && self.has_more(items_on_this_page)
    }

    /// Whether this page can be trusted to be the last one, or is merely the last one
    /// that had anything on it.
    ///
    /// `true` when the server sent a page count. Callers reporting progress use it to
    /// avoid claiming a total they are guessing at.
    #[must_use]
    pub fn total_is_known(self) -> bool {
        self.total_pages.is_some()
    }
}

/// One page of a paginated collection.
#[derive(Debug, Clone)]
pub struct Page<T> {
    /// The items on this page.
    pub items: Vec<T>,

    /// Where the page sits in the collection.
    pub info: PageInfo,
}

impl<T> Page<T> {
    /// Whether another page should be requested after this one.
    #[must_use]
    pub fn has_more(&self) -> bool {
        self.info.has_more(self.items.len())
    }
}

/// A cursor over a paginated collection.
///
/// Exists so a long fetch can report progress — the sync engine turns each page into a
/// `Msg` — without the API crate taking a dependency on a stream library. Use
/// [`Pager::collect_all`] when progress does not matter.
#[derive(Debug)]
pub struct Pager<T> {
    client: Client,
    call: Call,
    per_page: u32,
    /// What the server turned out to serve per page, when it does not say.
    effective_per_page: Option<u32>,
    next_page: Option<u32>,
    last: Option<PageInfo>,
    marker: PhantomData<fn() -> T>,
}

impl<T: DeserializeOwned> Pager<T> {
    /// Start at page 1 of the collection `call` addresses.
    pub(crate) fn new(client: Client, call: Call, per_page: u32) -> Self {
        Self {
            client,
            call,
            per_page: per_page.max(1),
            effective_per_page: None,
            next_page: Some(1),
            last: None,
            marker: PhantomData,
        }
    }

    /// The number of pages the server reported, once at least one page has been read.
    #[must_use]
    pub fn total_pages(&self) -> Option<u32> {
        self.last.and_then(|info| info.total_pages)
    }

    /// Fetch the next page, or `None` when the collection is exhausted.
    ///
    /// # Errors
    /// Any transport, status, or deserialization failure from the underlying request.
    pub async fn next_page(&mut self) -> Result<Option<Page<T>>> {
        let Some(page) = self.next_page else {
            return Ok(None);
        };

        let call = self
            .call
            .clone()
            .with_query("page", page.to_string())
            .with_query("per_page", self.per_page.to_string());

        let (items, headers) = self.client.send::<Vec<T>>(call).await?;
        let effective = self.effective_per_page.unwrap_or(self.per_page);
        let info = PageInfo::from_headers(&headers, page, self.per_page, effective);

        // With no page count to go on, the first page defines what a full page looks
        // like for the rest of the walk.
        if self.effective_per_page.is_none() && info.total_pages.is_none() && !items.is_empty() {
            self.effective_per_page = Some(items.len().min(self.per_page as usize) as u32);
        }

        if info.was_capped(items.len()) {
            tracing::debug!(
                requested = self.per_page,
                returned = items.len(),
                total_pages = ?info.total_pages,
                "server capped per_page; following its page count"
            );
        }

        if info.has_more(items.len()) && page >= MAX_PAGES {
            // A server ignoring `page` would otherwise spin here forever, or -- worse --
            // stop quietly and hand back a collection that looks complete.
            return Err(ApiError::TooManyPages {
                url: self.call.url_for_error(),
                pages: page,
            });
        }

        self.next_page = info.has_more(items.len()).then_some(page + 1);
        self.last = Some(info);
        Ok(Some(Page { items, info }))
    }

    /// Read every remaining page into one vector.
    ///
    /// # Errors
    /// Any failure from any page. A partial result is not returned: a half-loaded task
    /// list that looks complete is the failure mode this whole module exists to prevent.
    pub async fn collect_all(mut self) -> Result<Vec<T>> {
        let mut all = Vec::new();
        while let Some(page) = self.next_page().await? {
            all.extend(page.items);
        }
        Ok(all)
    }
}

/// Parse a numeric header, treating anything unparseable as absent.
fn header_u32(headers: &HeaderMap, name: &str) -> Option<u32> {
    headers.get(name)?.to_str().ok()?.trim().parse::<u32>().ok()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderMap, HeaderValue};

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            let name: reqwest::header::HeaderName = name.parse().unwrap();
            map.insert(name, HeaderValue::from_str(value).unwrap());
        }
        map
    }

    /// A page the server described with headers.
    fn described(pairs: &[(&str, &str)], page: u32, per_page: u32) -> PageInfo {
        PageInfo::from_headers(&headers(pairs), page, per_page, per_page)
    }

    /// A page from an endpoint that sent no pagination headers.
    fn undescribed(page: u32, requested: u32, effective: u32) -> PageInfo {
        PageInfo::from_headers(&HeaderMap::new(), page, requested, effective)
    }

    #[test]
    fn reads_both_pagination_headers() {
        let info = described(
            &[(TOTAL_PAGES_HEADER, "78"), (RESULT_COUNT_HEADER, "50")],
            1,
            50,
        );
        assert_eq!(info.total_pages, Some(78));
        assert_eq!(info.result_count, Some(50));
        assert!(info.total_is_known());
    }

    #[test]
    fn the_dev_instances_task_list_needs_every_page() {
        // 3,876 tasks at the servers' cap of 50 is 78 pages. Page 1 is not the answer.
        assert!(described(&[(TOTAL_PAGES_HEADER, "78")], 1, 50).has_more(50));
        assert!(!described(&[(TOTAL_PAGES_HEADER, "78")], 78, 50).has_more(26));
    }

    #[test]
    fn a_capped_page_size_is_detected_rather_than_believed() {
        // Ask for 10000 the way cria does; the server answers with 50 and says 78 pages.
        let info = described(
            &[(TOTAL_PAGES_HEADER, "78"), (RESULT_COUNT_HEADER, "50")],
            1,
            10_000,
        );
        assert!(info.was_capped(50));
        assert!(info.has_more(50));
    }

    #[test]
    fn without_headers_a_page_shorter_than_the_server_serves_is_the_last() {
        // The effective size is what the first page actually held, so this is the
        // ordinary case: full pages continue, a short one ends it.
        assert!(undescribed(1, 50, 50).has_more(50));
        assert!(!undescribed(2, 50, 50).has_more(49));
        assert!(!undescribed(2, 50, 50).has_more(0));
    }

    #[test]
    fn a_silent_cap_without_headers_does_not_truncate() {
        // The compound failure: no pagination headers *and* a server capping per_page
        // below what was asked. Comparing against the requested 10,000 would call page
        // one final and hand back a truncated collection reported as complete.
        // Comparing against what the server actually served -- 50 -- keeps walking.
        let info = undescribed(1, 10_000, 50);
        assert!(info.has_more(50));
        assert!(info.was_capped(50));
    }

    #[test]
    fn an_empty_page_always_ends_the_collection() {
        assert!(!undescribed(1, 50, 1).has_more(0));
        assert!(!undescribed(9, 50, 50).has_more(0));
    }

    #[test]
    fn unparseable_headers_are_treated_as_absent() {
        let info = described(
            &[(TOTAL_PAGES_HEADER, "lots"), (RESULT_COUNT_HEADER, "")],
            1,
            50,
        );
        assert_eq!(info.total_pages, None);
        assert_eq!(info.result_count, None);
        assert!(!info.total_is_known());
    }
}
