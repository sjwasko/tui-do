"""A terminal just complete enough to read what ratatui drew.

Only what crossterm actually emits: cursor positioning, erases, SGR (discarded),
and printable text. Enough to reconstruct the grid and read it back.
"""

import re


class Screen:
    def __init__(self, cols, rows):
        self.cols, self.rows = cols, rows
        self.grid = [[" "] * cols for _ in range(rows)]
        self.cx = self.cy = 0
        self.buf = ""

    def _put(self, ch):
        if 0 <= self.cy < self.rows and 0 <= self.cx < self.cols:
            self.grid[self.cy][self.cx] = ch
        self.cx += 1
        if self.cx >= self.cols:
            self.cx = 0
            self.cy = min(self.cy + 1, self.rows - 1)

    def _erase_display(self, mode):
        if mode == 2 or mode == 3:
            self.grid = [[" "] * self.cols for _ in range(self.rows)]
        elif mode == 0:
            for x in range(self.cx, self.cols):
                self.grid[self.cy][x] = " "
            for y in range(self.cy + 1, self.rows):
                self.grid[y] = [" "] * self.cols

    def _erase_line(self, mode):
        if mode == 0:
            for x in range(self.cx, self.cols):
                self.grid[self.cy][x] = " "
        elif mode == 1:
            for x in range(0, min(self.cx + 1, self.cols)):
                self.grid[self.cy][x] = " "
        else:
            self.grid[self.cy] = [" "] * self.cols

    def feed(self, text):
        self.buf += text
        i = 0
        s = self.buf
        while i < len(s):
            ch = s[i]
            if ch == "\x1b":
                if i + 1 >= len(s):
                    break  # incomplete, keep for next feed
                nxt = s[i + 1]
                if nxt == "[":
                    m = re.match(r"\x1b\[([0-9;?]*)([A-Za-z])", s[i:])
                    if not m:
                        break
                    params, final = m.group(1), m.group(2)
                    self._csi(params, final)
                    i += m.end()
                    continue
                if nxt == "]":  # OSC, runs to BEL or ST
                    m = re.match(r"\x1b\].*?(\x07|\x1b\\)", s[i:], re.S)
                    if not m:
                        break
                    i += m.end()
                    continue
                if nxt in "()#":
                    if i + 2 >= len(s):
                        break
                    i += 3
                    continue
                i += 2
                continue
            if ch == "\r":
                self.cx = 0
            elif ch == "\n":
                self.cy = min(self.cy + 1, self.rows - 1)
            elif ch == "\b":
                self.cx = max(self.cx - 1, 0)
            elif ch == "\t":
                self.cx = min(self.cx + 8 - (self.cx % 8), self.cols - 1)
            elif ch >= " ":
                self._put(ch)
            i += 1
        self.buf = s[i:]

    def _csi(self, params, final):
        if params.startswith("?"):
            return  # private modes: alt screen, cursor visibility
        nums = [int(p) for p in params.split(";") if p.isdigit()]
        first = nums[0] if nums else 0
        if final in "Hf":
            row = nums[0] if len(nums) > 0 else 1
            col = nums[1] if len(nums) > 1 else 1
            self.cy, self.cx = max(row - 1, 0), max(col - 1, 0)
        elif final == "A":
            self.cy = max(self.cy - max(first, 1), 0)
        elif final == "B":
            self.cy = min(self.cy + max(first, 1), self.rows - 1)
        elif final == "C":
            self.cx = min(self.cx + max(first, 1), self.cols - 1)
        elif final == "D":
            self.cx = max(self.cx - max(first, 1), 0)
        elif final == "J":
            self._erase_display(first)
        elif final == "K":
            self._erase_line(first)
        elif final == "G":
            self.cx = max(first - 1, 0)
        elif final == "d":
            self.cy = max(first - 1, 0)

    def text(self):
        return "\n".join("".join(row).rstrip() for row in self.grid)
