//! Small Micron -> HTML converter. Grammar follows NomadNet's reference
//! parser (`nomadnet/ui/textui/MicronParser.py`): headings (`>`, `>>`, ...),
//! horizontal dividers (lines starting with `-`), the `` `[label`url] `` link
//! syntax, backtick-toggled bold/italic/underline/color spans, and
//! `` `t ``-delimited tables. Forms, anchors and partials (NomadNet-specific,
//! not needed for read-only browsing) are not implemented.

pub fn micron_to_html(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::new();
    let mut literal = false;
    let mut table: Option<Vec<String>> = None;

    for raw_line in text.split('\n') {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);

        if line == "`=" {
            if literal {
                out.push_str("</pre>\n");
            } else {
                out.push_str("<pre>");
            }
            literal = !literal;
            continue;
        }
        if literal {
            out.push_str(&html_escape(line));
            out.push('\n');
            continue;
        }

        // A leading backslash escapes the rest of the line's first character,
        // so `\` followed by a backtick renders the backtick literally instead
        // of opening a formatting run (MicronParser.py `pre_escape`).
        let (line, pre_escape) = match line.strip_prefix('\\') {
            Some(rest) => (rest, true),
            None => (line, false),
        };

        if !pre_escape && line.starts_with('#') {
            continue;
        }

        if let Some(rest) = line.strip_prefix("`t") {
            match table.take() {
                Some(rows) => out.push_str(&render_table(&rows)),
                None => table = Some(Vec::new()),
            }
            let _ = rest;
            continue;
        }
        if let Some(rows) = table.as_mut() {
            rows.push(line.to_string());
            continue;
        }

        // `` `{url`refresh`fields} `` — a partial. We don't fetch partials, but
        // the line must not fall through to inline parsing, which would consume
        // the backticks as formatting and spill the URL into visible text.
        if let Some(rest) = line.strip_prefix("`{") {
            if let Some(partial) = render_partial(rest) {
                out.push_str(&partial);
                continue;
            }
        }

        // A leading `<` resets section depth; the rest of the line is parsed
        // normally rather than showing a stray `<`.
        let line = if !pre_escape {
            line.strip_prefix('<').unwrap_or(line)
        } else {
            line
        };

        if !pre_escape && let Some(depth) = heading_depth(line) {
            let level = depth.min(6);
            let (html, align) = parse_inline(&line[depth..], false);
            out.push_str(&format!(
                "<h{level}{}>{html}</h{level}>\n",
                align_attr(align)
            ));
            continue;
        }

        if !pre_escape && line.starts_with('-') {
            out.push_str(&render_divider(line));
            continue;
        }

        if line.trim().is_empty() {
            continue;
        }

        let (html, align) = parse_inline(line, pre_escape);
        out.push_str(&format!("<p{}>{html}</p>\n", align_attr(align)));
    }

    if let Some(rows) = table.take() {
        out.push_str(&render_table(&rows));
    }

    out
}

/// Number of divider characters drawn for a `-X` rule, matching the reference
/// renderer's fixed run (the container clips the overflow).
const DIVIDER_RUN: usize = 250;

/// Dividers per `MicronParser.py:325-336`: a bare `-` is a plain rule, `-X`
/// repeats `X`, and anything longer repeats the default box-drawing dash.
/// Control characters are rejected as the divider glyph, as upstream does.
fn render_divider(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    if chars.len() == 1 {
        return String::from("<hr>\n");
    }
    let glyph = if chars.len() == 2 && !chars[1].is_control() {
        chars[1]
    } else {
        '\u{2500}'
    };
    let run: String = std::iter::repeat(glyph).take(DIVIDER_RUN).collect();
    format!(
        "<div class=\"mu-divider\" style=\"white-space:nowrap;overflow:hidden\">{}</div>\n",
        html_escape(&run)
    )
}

/// Renders the `` `{url`refresh`fields} `` partial placeholder. The content
/// isn't fetched — the element carries the descriptor so a caller could — but
/// it renders as the reference's `⧖` marker rather than leaking raw markup.
fn render_partial(rest: &str) -> Option<String> {
    let end = rest.find('}')?;
    let data = &rest[..end];
    let mut parts = data.split('`');
    let url = parts.next().unwrap_or("");
    if url.is_empty() {
        return None;
    }
    let refresh = parts.next().unwrap_or("");
    let fields = parts.next().unwrap_or("");

    let mut attrs = format!(" data-partial-url=\"{}\"", html_escape(url));
    if refresh.parse::<f64>().is_ok_and(|r| r >= 1.0) {
        attrs.push_str(&format!(" data-partial-refresh=\"{}\"", html_escape(refresh)));
    }
    if !fields.is_empty() {
        attrs.push_str(&format!(" data-partial-fields=\"{}\"", html_escape(fields)));
    }
    Some(format!("<div class=\"mu-partial\"{attrs}>\u{29d6}</div>\n"))
}

fn heading_depth(line: &str) -> Option<usize> {
    let depth = line.chars().take_while(|&c| c == '>').count();
    if depth > 0 && line.len() > depth {
        Some(depth)
    } else {
        None
    }
}

fn align_attr(align: Option<&'static str>) -> String {
    match align {
        Some(a) => format!(" style=\"text-align:{a}\""),
        None => String::new(),
    }
}

#[derive(Default)]
struct InlineState {
    bold: bool,
    underline: bool,
    italic: bool,
    fg: Option<String>,
    bg: Option<String>,
}

impl InlineState {
    fn css(&self) -> String {
        let mut decls = Vec::new();
        if self.bold {
            decls.push("font-weight:bold".to_string());
        }
        if self.italic {
            decls.push("font-style:italic".to_string());
        }
        if self.underline {
            decls.push("text-decoration:underline".to_string());
        }
        if let Some(fg) = &self.fg {
            decls.push(format!("color:{fg}"));
        }
        if let Some(bg) = &self.bg {
            decls.push(format!("background-color:{bg}"));
        }
        decls.join(";")
    }

    fn reset(&mut self) {
        *self = InlineState::default();
    }
}

/// Parses one line's inline formatting/link syntax. Returns the HTML and the
/// last explicit alignment directive seen (`` `c``/`` `l``/`` `r``), if any.
fn parse_inline(line: &str, pre_escape: bool) -> (String, Option<&'static str>) {
    let mut out = String::new();
    let mut part = String::new();
    let mut state = InlineState::default();
    let mut align: Option<&'static str> = None;

    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    let mut in_formatting = false;
    // Set by a leading `\` (or a mid-line one): the next character is taken
    // literally instead of being read as markup.
    let mut escape = pre_escape;

    while i < chars.len() {
        let c = chars[i];

        if !in_formatting {
            if escape {
                part.push(c);
                escape = false;
            } else if c == '\\' {
                escape = true;
            } else if c == '`' {
                flush(&mut out, &mut part, &state);
                in_formatting = true;
            } else {
                part.push(c);
            }
            i += 1;
            continue;
        }

        in_formatting = false;
        match c {
            '_' => state.underline = !state.underline,
            '!' => state.bold = !state.bold,
            '*' => state.italic = !state.italic,
            '`' => state.reset(),
            'c' => align = Some("center"),
            'l' => align = Some("left"),
            'r' => align = Some("right"),
            'a' => align = None,
            'f' => state.fg = None,
            'b' => state.bg = None,
            'F' | 'B' => {
                let (color, consumed) = read_color(&chars[i + 1..]);
                if c == 'F' {
                    state.fg = color;
                } else {
                    state.bg = color;
                }
                i += consumed;
            }
            // `` `:name `` — a zero-width anchor. The name is markup, not text:
            // emitting it inline used to leak the anchor name into the page.
            ':' => {
                let start = i + 1;
                let mut end = start;
                while end < chars.len() && (chars[end].is_alphanumeric() || matches!(chars[end], '_' | '-')) {
                    end += 1;
                }
                if end > start {
                    flush(&mut out, &mut part, &state);
                    let name: String = chars[start..end].iter().collect();
                    out.push_str(&format!(
                        "<a class=\"mu-anchor\" id=\"{}\" aria-hidden=\"true\"></a>",
                        html_escape(&name)
                    ));
                    i = end;
                    continue;
                }
            }
            '[' => {
                if let Some(end) = chars[i + 1..].iter().position(|&c| c == ']') {
                    let link_data: String = chars[i + 1..i + 1 + end].iter().collect();
                    let mut parts = link_data.splitn(3, '`');
                    let first = parts.next().unwrap_or("");
                    let url = parts.next();
                    let (label, url) = match url {
                        Some(url) => (first, url),
                        None => (first, first),
                    };
                    out.push_str("<a href=\"");
                    out.push_str(&html_escape(&sanitize_url(url)));
                    out.push_str("\">");
                    out.push_str(&html_escape(label));
                    out.push_str("</a>");
                    i += 1 + end;
                }
            }
            _ => {}
        }
        i += 1;
    }

    flush(&mut out, &mut part, &state);
    (out, align)
}

fn flush(out: &mut String, part: &mut String, state: &InlineState) {
    if part.is_empty() {
        return;
    }
    let css = state.css();
    if css.is_empty() {
        out.push_str(&html_escape(part));
    } else {
        out.push_str("<span style=\"");
        // Escaped as well as validated at the source (`read_color`), so a
        // future colour format can't silently reopen attribute injection.
        out.push_str(&html_escape(&css));
        out.push_str("\">");
        out.push_str(&html_escape(part));
        out.push_str("</span>");
    }
    part.clear();
}

/// Reads a Micron color code (3-hex shorthand, `gNN` grayscale, or `T` +
/// 6-hex truecolor) starting right after the `` `F``/`` `B`` marker. Returns
/// the CSS color and how many extra chars (beyond the `F`/`B` itself) were consumed.
fn read_color(rest: &[char]) -> (Option<String>, usize) {
    if rest.first() == Some(&'T') && rest.len() >= 7 {
        // Must be validated as hex: these characters end up inside a
        // `style="..."` attribute, so accepting them verbatim lets remote
        // content close the attribute and inject markup (`` `FT"><img ``).
        if rest[1..7].iter().all(|c| c.is_ascii_hexdigit()) {
            let hex: String = rest[1..7].iter().collect();
            return (Some(format!("#{hex}")), 7);
        }
        return (None, 7);
    }
    if rest.len() >= 3 {
        let code: String = rest[..3].iter().collect();
        if let Some(gray) = code.strip_prefix('g') {
            if let Ok(pct) = gray.parse::<u32>() {
                let v = (pct.min(99) * 255) / 99;
                return (Some(format!("rgb({v},{v},{v})")), 3);
            }
        }
        if code.chars().all(|c| c.is_ascii_hexdigit()) {
            let doubled: String = code.chars().flat_map(|c| [c, c]).collect();
            return (Some(format!("#{doubled}")), 3);
        }
    }
    (None, 0)
}

/// Neutralises link targets that would execute script when followed. Micron
/// pages come from untrusted remote nodes and are rendered in a real webview,
/// where `` `[click`javascript:...] `` would otherwise run in the frame — the
/// `nomad://` responses carry no CSP of their own to fall back on.
///
/// Relative paths (`/page/x.mu`) and ordinary network schemes are left alone;
/// only script-bearing schemes are rejected. Leading control/whitespace bytes
/// are stripped first because browsers ignore them when resolving the scheme,
/// so `"java\tscript:..."` would otherwise slip through a naive prefix check.
fn sanitize_url(url: &str) -> String {
    let normalised: String = url
        .chars()
        .filter(|c| !c.is_whitespace() && !c.is_control())
        .collect::<String>()
        .to_ascii_lowercase();

    const BLOCKED: [&str; 3] = ["javascript:", "data:", "vbscript:"];
    if BLOCKED.iter().any(|s| normalised.starts_with(s)) {
        return String::from("#blocked");
    }
    rewrite_cross_node_url(url)
}

/// Length of a NomadNet destination hash in hex characters (16 bytes).
const DEST_HASH_HEX_LEN: usize = 32;

/// NomadNet's `Browser.DEFAULT_PATH`, used when a link names a node with no path.
const NOMAD_DEFAULT_PAGE_PATH: &str = "/page/index.mu";

/// Maps NomadNet's cross-node link forms onto the `nomad://` scheme the
/// webview can actually navigate.
///
/// A page hosted elsewhere is linked either as `<dest-hash>:/page/x.mu` or as
/// `nomadnetwork://<dest-hash>/page/x.mu`. Left alone, the first is parsed by
/// the browser as an unregistered URL *scheme* (`<dest-hash>:`) and the second
/// as an unregistered scheme too — so the click silently does nothing. Links
/// within the current node are relative and already resolve against the
/// frame's origin, so they are untouched.
fn rewrite_cross_node_url(url: &str) -> String {
    if let Some(rest) = url.strip_prefix("nomadnetwork://") {
        return format!("nomad://{rest}");
    }

    let is_dest_hash = |s: &str| {
        s.len() == DEST_HASH_HEX_LEN && s.chars().all(|c| c.is_ascii_hexdigit())
    };

    // NomadNet splits a link target on `:` (Browser.py `retrieve_url`):
    //   `<hash>`         → that node, default path
    //   `<hash>:<path>`  → that node, that path
    //   `:<path>`        → *this* node, that path
    // Anything else is malformed there, so it is left untouched here rather
    // than guessed at.
    match url.split_once(':') {
        Some((first, path)) if is_dest_hash(first) => {
            let path = if path.is_empty() {
                NOMAD_DEFAULT_PAGE_PATH.to_string()
            } else if path.starts_with('/') {
                path.to_string()
            } else {
                format!("/{path}")
            };
            format!("nomad://{first}{path}")
        }
        // Empty first component: same node. Emitting the path alone lets the
        // browser resolve it against the frame's own origin. Keeping the
        // leading `:` would instead produce `nomad://<host>/:/page/x.mu`.
        Some(("", path)) if !path.is_empty() && !path.contains(':') => path.to_string(),
        _ if is_dest_hash(url) => format!("nomad://{url}{NOMAD_DEFAULT_PAGE_PATH}"),
        _ => url.to_string(),
    }
}

fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

/// Simplified table: buffered lines split on `|`; a second row made only of
/// `-`/`:` cells (GFM-style separator) marks the first row as a header.
fn render_table(lines: &[String]) -> String {
    let rows: Vec<Vec<String>> = lines
        .iter()
        .map(|line| {
            line.trim_matches('|')
                .split('|')
                .map(|cell| cell.trim().to_string())
                .collect()
        })
        .filter(|row: &Vec<String>| !(row.len() == 1 && row[0].is_empty()))
        .collect();

    if rows.is_empty() {
        return String::new();
    }

    let is_separator = |row: &[String]| {
        !row.is_empty() && row.iter().all(|c| !c.is_empty() && c.chars().all(|ch| ch == '-' || ch == ':'))
    };

    let mut out = String::from("<table>\n");
    let has_header = rows.len() > 1 && is_separator(&rows[1]);
    let body_start = if has_header {
        if let Some(first) = rows.first() {
            out.push_str("<thead><tr>");
            for cell in first {
                out.push_str("<th>");
                out.push_str(&html_escape(cell));
                out.push_str("</th>");
            }
            out.push_str("</tr></thead>\n");
        }
        2
    } else {
        0
    };

    out.push_str("<tbody>\n");
    for row in rows.iter().skip(body_start) {
        out.push_str("<tr>");
        for cell in row {
            out.push_str("<td>");
            out.push_str(&html_escape(cell));
            out.push_str("</td>");
        }
        out.push_str("</tr>\n");
    }
    out.push_str("</tbody></table>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_heading_levels() {
        assert_eq!(micron_to_html(b">H1"), "<h1>H1</h1>\n");
        assert_eq!(micron_to_html(b">>H2"), "<h2>H2</h2>\n");
        assert_eq!(micron_to_html(b">>>H3"), "<h3>H3</h3>\n");
    }

    #[test]
    fn renders_plain_paragraph() {
        assert_eq!(micron_to_html(b"hello world"), "<p>hello world</p>\n");
    }

    #[test]
    fn renders_bold_and_underline_toggle_spans() {
        assert_eq!(
            micron_to_html(b"`!bold`! plain `_under`_"),
            "<p><span style=\"font-weight:bold\">bold</span> plain <span style=\"text-decoration:underline\">under</span></p>\n"
        );
    }

    #[test]
    fn renders_foreground_color_span() {
        assert_eq!(
            micron_to_html(b"`Ff00red`f"),
            "<p><span style=\"color:#ff0000\">red</span></p>\n"
        );
    }

    // --- Divergence #5: dividers (MicronParser.py:325-336) ----------------
    #[test]
    fn renders_divider() {
        // Bare `-` is a plain rule.
        assert_eq!(micron_to_html(b"-"), "<hr>\n");
    }

    #[test]
    fn divider_with_glyph_repeats_that_glyph() {
        let html = micron_to_html("-\u{223f}".as_bytes());
        assert!(html.contains("mu-divider"), "got: {html}");
        assert_eq!(html.matches('\u{223f}').count(), DIVIDER_RUN, "got: {html}");
        assert!(!html.contains("<hr>"), "should not collapse to <hr>: {html}");
    }

    #[test]
    fn long_divider_uses_default_glyph() {
        let html = micron_to_html(b"---");
        assert_eq!(html.matches('\u{2500}').count(), DIVIDER_RUN, "got: {html}");
        assert!(!html.contains("<hr>"), "should not collapse to <hr>: {html}");
    }

    #[test]
    fn divider_glyph_is_escaped() {
        let html = micron_to_html(b"-<");
        assert!(!html.contains(" <<"), "unescaped glyph: {html}");
        assert!(html.contains("&lt;"), "got: {html}");
    }

    // --- Divergence #1: backslash line-escape -----------------------------
    #[test]
    fn leading_backslash_escapes_formatting() {
        // Was: `\<span style="font-weight:bold">literal backtick</span>` —
        // backslash shown *and* the run still styled.
        assert_eq!(
            micron_to_html(b"\\`!literal backtick"),
            "<p>`!literal backtick</p>\n"
        );
    }

    #[test]
    fn mid_line_backslash_escapes_next_char() {
        assert_eq!(micron_to_html(b"a\\`!b"), "<p>a`!b</p>\n");
    }

    #[test]
    fn leading_backslash_defuses_line_markers() {
        // Escaped first char means these are text, not heading/comment/divider.
        assert_eq!(micron_to_html(b"\\# not a comment"), "<p># not a comment</p>\n");
        assert_eq!(micron_to_html(b"\\>not a heading"), "<p>&gt;not a heading</p>\n");
    }

    // --- Divergence #2: `:anchor ------------------------------------------
    #[test]
    fn anchor_does_not_leak_name_into_text() {
        let html = micron_to_html(b"`:myanchor jump target");
        assert!(html.contains("id=\"myanchor\""), "got: {html}");
        assert!(
            !html.contains(">myanchor"),
            "anchor name leaked into text: {html}"
        );
        assert!(html.contains("jump target"), "got: {html}");
    }

    #[test]
    fn anchor_name_is_escaped() {
        let html = micron_to_html(b"`:a-b_1 text");
        assert!(html.contains("id=\"a-b_1\""), "got: {html}");
    }

    // --- Divergence #3: `{...} partials -----------------------------------
    #[test]
    fn partial_renders_placeholder_not_garbage() {
        // Was: `<p>/page/live.muid=x}</p>` — URL and fields leaked as text.
        let html = micron_to_html(b"`{/page/live.mu`5`pid=x}");
        assert!(html.contains("mu-partial"), "got: {html}");
        assert!(html.contains('\u{29d6}'), "missing placeholder glyph: {html}");
        assert!(
            html.contains("data-partial-url=\"/page/live.mu\""),
            "got: {html}"
        );
        assert!(html.contains("data-partial-refresh=\"5\""), "got: {html}");
        assert!(!html.contains("id=x}"), "raw field text leaked: {html}");
    }

    #[test]
    fn partial_without_refresh_or_fields() {
        let html = micron_to_html(b"`{/page/x.mu}");
        assert!(html.contains("data-partial-url=\"/page/x.mu\""), "got: {html}");
        assert!(!html.contains("data-partial-refresh"), "got: {html}");
    }

    // --- Divergence #4: `<` line-start depth reset -------------------------
    #[test]
    fn leading_angle_bracket_consumed_as_depth_reset() {
        // Was: `<p>&lt;reset depth</p>` — stray `<` shown to the user.
        assert_eq!(micron_to_html(b"<reset depth"), "<p>reset depth</p>\n");
    }

    #[test]
    fn renders_link() {
        assert_eq!(
            micron_to_html(b"`[Click here`/page/other.mu]"),
            "<p><a href=\"/page/other.mu\">Click here</a></p>\n"
        );
    }

    #[test]
    fn renders_link_without_label_uses_url_as_label() {
        assert_eq!(
            micron_to_html(b"`[/page/other.mu]"),
            "<p><a href=\"/page/other.mu\">/page/other.mu</a></p>\n"
        );
    }

    #[test]
    fn renders_simple_table_with_header() {
        let mu = b"`t\nName|Hops\n-|-\nAlice|2\n`t";
        let html = micron_to_html(mu);
        assert!(html.contains("<thead><tr><th>Name</th><th>Hops</th></tr></thead>"));
        assert!(html.contains("<tr><td>Alice</td><td>2</td></tr>"));
    }

    #[test]
    fn skips_comment_lines() {
        assert_eq!(micron_to_html(b"# a comment\nreal text"), "<p>real text</p>\n");
    }

    #[test]
    fn renders_literal_block_unparsed() {
        assert_eq!(
            micron_to_html(b"`=\n`!not bold`!\n`="),
            "<pre>`!not bold`!\n</pre>\n"
        );
    }

    // Ground truth from nomad-core's `generate_index` (rsNodePage
    // crates/nomad-core/src/pages.rs): `>Pages\n\n` then either
    // "No pages have been published yet.\n" or one `` `[name`/page/name] `` per line.
    // --- Injection regression tests -------------------------------------
    // Micron is fetched from untrusted remote nodes and rendered in a real
    // webview, so every one of these must stay inert.

    #[test]
    fn escapes_markup_in_ordinary_text() {
        let html = micron_to_html(b"x <script>alert(1)</script> <img src=x onerror=alert(2)>");
        assert!(!html.contains("<script"), "raw <script> survived: {html}");
        assert!(!html.contains("<img"), "raw <img> survived: {html}");
        assert!(html.contains("&lt;script&gt;"));
    }

    #[test]
    fn leading_angle_bracket_is_depth_reset_and_stays_inert() {
        // A leading `<` is the section-depth reset marker and is consumed
        // (MicronParser.py), so this does not round-trip as text — but it must
        // still never produce live markup.
        let html = micron_to_html(b"<script>alert(1)</script>");
        assert!(!html.contains("<script"), "raw <script> survived: {html}");
        assert!(html.contains("script&gt;alert(1)&lt;/script&gt;"), "got: {html}");
    }

    #[test]
    fn escapes_markup_in_heading() {
        let html = micron_to_html(b"><script>alert(1)</script>");
        assert!(!html.contains("<script"), "raw <script> survived: {html}");
    }

    #[test]
    fn truecolor_directive_cannot_break_out_of_style_attribute() {
        // Regression: the truecolor branch used to take 6 chars verbatim, so
        // `"><img` closed the style attribute and injected an element.
        let html = micron_to_html(b"`FT\"><img PWNED");
        assert!(!html.contains("<img"), "attribute injection: {html}");
        assert!(!html.contains("\"><"), "attribute injection: {html}");
    }

    #[test]
    fn truecolor_directive_still_accepts_valid_hex() {
        assert_eq!(
            micron_to_html(b"`FT00ff00green"),
            "<p><span style=\"color:#00ff00\">green</span></p>\n"
        );
    }

    #[test]
    fn rejects_script_bearing_link_schemes() {
        for mu in [
            &b"`[x`javascript:alert(1)]"[..],
            &b"`[x`JaVaScRiPt:alert(1)]"[..],
            &b"`[x`java\tscript:alert(1)]"[..],
            &b"`[x`data:text/html,<script>alert(1)</script>]"[..],
            &b"`[x`vbscript:msgbox]"[..],
        ] {
            let html = micron_to_html(mu);
            assert!(
                html.contains("href=\"#blocked\""),
                "dangerous scheme not blocked for {:?}: {html}",
                String::from_utf8_lossy(mu)
            );
        }
    }

    // --- Cross-node links must become navigable `nomad://` URLs ------------
    #[test]
    fn rewrites_hash_colon_cross_node_link() {
        // Was `href="a1b2…:/page/index.mu"`, which the browser reads as an
        // unregistered URL *scheme* — the click silently did nothing.
        let html = micron_to_html(
            b"`[Other`a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6:/page/index.mu]",
        );
        assert!(
            html.contains("href=\"nomad://a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6/page/index.mu\""),
            "got: {html}"
        );
    }

    #[test]
    fn rewrites_nomadnetwork_scheme_cross_node_link() {
        let html = micron_to_html(
            b"`[Other`nomadnetwork://a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6/page/index.mu]",
        );
        assert!(
            html.contains("href=\"nomad://a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6/page/index.mu\""),
            "got: {html}"
        );
    }

    #[test]
    fn cross_node_link_without_leading_slash_gets_one() {
        let html =
            micron_to_html(b"`[Other`a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6:page/index.mu]");
        assert!(
            html.contains("href=\"nomad://a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6/page/index.mu\""),
            "got: {html}"
        );
    }

    #[test]
    fn non_hash_scheme_like_targets_are_left_alone() {
        // Not 32 hex chars — must not be mistaken for a destination hash.
        assert!(micron_to_html(b"`[m`mailto:a@b.c]").contains("href=\"mailto:a@b.c\""));
        assert!(
            micron_to_html(b"`[h`https://example.org/a]")
                .contains("href=\"https://example.org/a\"")
        );
    }

    // --- Real-world link forms captured from live nodes ---------------
    // Source: INGEN KONG - Resist (31e1c61c…) /page/index.mu, fetched live.
    #[test]
    fn rewrites_same_node_colon_prefixed_link() {
        // `[Main page`:/page/index.mu] — empty first component means "this
        // node" (Browser.py retrieve_url). Previously emitted
        // href=":/page/index.mu", which resolved to nomad://<host>/:/page/...
        let html = micron_to_html(b"`[Main page`:/page/index.mu]");
        assert!(html.contains("href=\"/page/index.mu\""), "got: {html}");
        assert!(!html.contains("\":/page"), "leading colon survived: {html}");
    }

    #[test]
    fn rewrites_cross_node_colon_prefixed_link() {
        // `[The Library`:a2d4…:/page/index.mu] — hash + path.
        let html = micron_to_html(
            b"`[The Library`a2d4202e63899b472449c27d3e951257:/page/index.mu]",
        );
        assert!(
            html.contains("href=\"nomad://a2d4202e63899b472449c27d3e951257/page/index.mu\""),
            "got: {html}"
        );
    }

    #[test]
    fn bare_hash_link_uses_default_path() {
        // `[3881f480…] — a node with no path; NomadNet uses DEFAULT_PATH.
        let html = micron_to_html(b"`[3881f480a71de26a8c2e6d637220f384]");
        assert!(
            html.contains("href=\"nomad://3881f480a71de26a8c2e6d637220f384/page/index.mu\""),
            "got: {html}"
        );
    }

    #[test]
    fn leaves_non_conforming_link_targets_untouched() {
        // lxmf@… and web URLs are not node links; NomadNet treats them as
        // malformed rather than navigating, so they must not be rewritten.
        assert!(micron_to_html(b"`[lxmf@7874a9d887f1d967b397d7577b47bdb8]")
            .contains("href=\"lxmf@7874a9d887f1d967b397d7577b47bdb8\""));
        assert!(micron_to_html(b"`[RT Radio`:https://codeberg.org/x]")
            .contains("href=\":https://codeberg.org/x\""));
    }

    #[test]
    fn keeps_ordinary_link_targets_intact() {
        assert!(micron_to_html(b"`[x`/page/a.mu]").contains("href=\"/page/a.mu\""));
        assert!(micron_to_html(b"`[x`https://example.org/a]").contains("href=\"https://example.org/a\""));
    }

    #[test]
    fn renders_nomad_core_generated_index_empty() {
        let mu = b">Pages\n\nNo pages have been published yet.\n";
        let html = micron_to_html(mu);
        assert_eq!(
            html,
            "<h1>Pages</h1>\n<p>No pages have been published yet.</p>\n"
        );
    }

    #[test]
    fn renders_nomad_core_generated_index_with_pages() {
        let mu = b">Pages\n\n`[index.mu`/page/index.mu]\n`[about.mu`/page/about.mu]\n";
        let html = micron_to_html(mu);
        assert_eq!(
            html,
            "<h1>Pages</h1>\n\
             <p><a href=\"/page/index.mu\">index.mu</a></p>\n\
             <p><a href=\"/page/about.mu\">about.mu</a></p>\n"
        );
    }
}

