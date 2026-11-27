#[derive(Clone, Copy, PartialEq, Eq)]
enum ForeignNs {
    Svg,
    Math,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum IntegrationKind {
    None,
    Html,
    MathText,
}

#[derive(Clone, PartialEq, Eq)]
enum StackEntry {
    Html(Vec<u8>),
    Foreign {
        ns: ForeignNs,
        tag: Vec<u8>,
        integration: IntegrationKind,
    },
    XmlScript(Vec<u8>),
}

#[derive(Default)]
pub(super) struct NsStack {
    stack: Vec<StackEntry>,
    xhtml: bool,
}

impl NsStack {
    pub(super) fn new(xhtml: bool) -> Self {
        Self {
            stack: Vec::new(),
            xhtml,
        }
    }

    pub(super) fn is_xhtml(&self) -> bool {
        self.xhtml
    }

    pub(super) fn in_foreign_namespace(&self) -> bool {
        matches!(self.stack.last(), Some(StackEntry::Foreign { .. }))
    }

    pub(super) fn in_foreign_for_start_tag(&self, tag_name: &[u8]) -> bool {
        match self.stack.last() {
            Some(StackEntry::Foreign {
                integration: IntegrationKind::None,
                ..
            }) => true,
            Some(StackEntry::Foreign {
                ns: ForeignNs::Math,
                integration: IntegrationKind::MathText,
                ..
            }) => {
                tag_name.eq_ignore_ascii_case(b"mglyph")
                    || tag_name.eq_ignore_ascii_case(b"malignmark")
            }
            _ => false,
        }
    }

    fn in_foreign_content_point(&self) -> bool {
        matches!(
            self.stack.last(),
            Some(StackEntry::Foreign {
                integration: IntegrationKind::None,
                ..
            })
        )
    }

    pub(super) fn suppressing_script(&self) -> bool {
        self.stack.iter().any(|e| match e {
            StackEntry::XmlScript(_) => true,
            StackEntry::Foreign { tag, .. } => tag.as_slice() == b"script",
            StackEntry::Html(_) => false,
        })
    }

    pub(super) fn in_select_in_table(&self) -> bool {
        let mut seen_select = false;
        let mut idx = self.stack.len();
        while idx > 0 {
            idx -= 1;
            match &self.stack[idx] {
                StackEntry::Html(open) if open == b"select" && !seen_select => {
                    seen_select = true;
                }
                StackEntry::Html(open) if matches!(open.as_slice(), b"html" | b"template") => {
                    return false;
                }
                StackEntry::Html(open) if seen_select && is_table_scope_end_tag(open) => {
                    return true;
                }
                StackEntry::Html(open) if !seen_select && is_table_scope_end_tag(open) => {
                    return false;
                }
                StackEntry::Html(_) => continue,
                _ => return false,
            }
        }
        false
    }

    pub(super) fn in_select_scope(&self) -> bool {
        let mut idx = self.stack.len();
        while idx > 0 {
            idx -= 1;
            match &self.stack[idx] {
                StackEntry::Html(open) if open == b"select" => return true,
                StackEntry::Html(open)
                    if matches!(
                        open.as_slice(),
                        b"html"
                            | b"table"
                            | b"tbody"
                            | b"thead"
                            | b"tfoot"
                            | b"tr"
                            | b"td"
                            | b"th"
                            | b"caption"
                            | b"template"
                    ) =>
                {
                    return false;
                }
                StackEntry::Html(_) => continue,
                _ => return false,
            }
        }
        false
    }

    pub(super) fn pop_select_in_scope(&mut self) {
        let mut idx = self.stack.len();
        while idx > 0 {
            idx -= 1;
            if matches!(&self.stack[idx], StackEntry::Html(open) if open == b"select") {
                self.stack.truncate(idx);
                return;
            }
        }
    }

    pub(super) fn on_start_tag(
        &mut self,
        tag_name: &[u8],
        tag_attrs_raw: &str,
        self_closing: bool,
        handled_synchronously: bool,
    ) {
        if self.xhtml {
            self.on_xhtml_start_tag(tag_name, self_closing, handled_synchronously);
            return;
        }
        let breaks_out = is_foreign_breakout_tag(tag_name)
            || (tag_name.eq_ignore_ascii_case(b"font") && font_has_breakout_attr(tag_attrs_raw));
        if self.in_foreign_for_start_tag(tag_name) && breaks_out {
            while self.in_foreign_content_point() {
                self.stack.pop();
            }
        }
        if self.in_foreign_for_start_tag(tag_name) {
            if self_closing {
                return;
            }
            let ns = match self.stack.last() {
                Some(StackEntry::Foreign { ns, .. }) => *ns,
                _ => ForeignNs::Svg,
            };
            let integration = foreign_integration_kind(ns, tag_name, tag_attrs_raw);
            self.stack.push(StackEntry::Foreign {
                ns,
                tag: tag_name.to_ascii_lowercase(),
                integration,
            });
            return;
        }

        let lower = tag_name.to_ascii_lowercase();
        self.close_implicit_html_before_start_tag(&lower);
        if lower == b"svg" {
            if !self_closing {
                self.stack.push(StackEntry::Foreign {
                    ns: ForeignNs::Svg,
                    tag: lower,
                    integration: IntegrationKind::None,
                });
            }
            return;
        }
        if lower == b"math" {
            if !self_closing {
                self.stack.push(StackEntry::Foreign {
                    ns: ForeignNs::Math,
                    tag: lower,
                    integration: IntegrationKind::None,
                });
            }
            return;
        }
        if !handled_synchronously && !is_html_void_element(&lower) {
            self.stack.push(StackEntry::Html(lower));
        }
    }

    fn on_xhtml_start_tag(
        &mut self,
        tag_name: &[u8],
        self_closing: bool,
        handled_synchronously: bool,
    ) {
        if self_closing {
            return;
        }
        let local = super::xml::local_name(tag_name, true);
        if local.eq_ignore_ascii_case(b"script") {
            self.stack
                .push(StackEntry::XmlScript(tag_name.to_ascii_lowercase()));
        } else if local.eq_ignore_ascii_case(b"svg") {
            self.stack.push(StackEntry::Foreign {
                ns: ForeignNs::Svg,
                tag: local.to_ascii_lowercase(),
                integration: IntegrationKind::None,
            });
        } else if local.eq_ignore_ascii_case(b"math") {
            self.stack.push(StackEntry::Foreign {
                ns: ForeignNs::Math,
                tag: local.to_ascii_lowercase(),
                integration: IntegrationKind::None,
            });
        } else if !handled_synchronously {
            self.stack
                .push(StackEntry::Html(tag_name.to_ascii_lowercase()));
        }
    }

    fn close_implicit_html_before_start_tag(&mut self, lower_tag: &[u8]) {
        if closes_p_in_button_scope(lower_tag) {
            self.pop_html_if_in_custom_scope(b"p", &[b"button"]);
        }
        match lower_tag {
            b"li" => self.pop_html_if_in_custom_scope(b"li", &[b"ol", b"ul"]),
            b"dd" | b"dt" => {
                let mut idx = self.stack.len();
                while idx > 0 {
                    idx -= 1;
                    match &self.stack[idx] {
                        StackEntry::Foreign {
                            ns,
                            tag,
                            integration,
                        } if is_foreign_html_scope_barrier(*ns, tag, *integration) => {
                            break;
                        }
                        StackEntry::Foreign { .. } | StackEntry::XmlScript(_) => continue,
                        StackEntry::Html(open) if matches!(open.as_slice(), b"dd" | b"dt") => {
                            self.stack.truncate(idx);
                            break;
                        }
                        StackEntry::Html(open)
                            if is_html_scope_barrier(open)
                                || (!matches!(open.as_slice(), b"div")
                                    && is_special_non_inline(open)) =>
                        {
                            break;
                        }
                        StackEntry::Html(_) => {}
                    }
                }
            }
            b"option" => {
                if matches!(self.stack.last(), Some(StackEntry::Html(open)) if open == b"option") {
                    self.stack.pop();
                }
            }
            b"optgroup" => {
                if matches!(self.stack.last(), Some(StackEntry::Html(open)) if open == b"option") {
                    self.stack.pop();
                }
                if matches!(self.stack.last(), Some(StackEntry::Html(open)) if open == b"optgroup")
                {
                    self.stack.pop();
                }
            }
            b"td" | b"th" => self.pop_table_cells_to(&[b"tr", b"table", b"template", b"html"]),
            b"tr" => self.pop_table_cells_to(&[
                b"tbody",
                b"thead",
                b"tfoot",
                b"table",
                b"template",
                b"html",
            ]),
            b"tbody" | b"thead" | b"tfoot" => {
                self.pop_table_cells_to(&[b"table", b"template", b"html"]);
            }
            _ => {}
        }
    }

    fn pop_html_if_in_custom_scope(&mut self, target: &[u8], extra_barriers: &[&[u8]]) {
        let mut idx = self.stack.len();
        while idx > 0 {
            idx -= 1;
            match &self.stack[idx] {
                StackEntry::Foreign {
                    ns,
                    tag,
                    integration,
                } if is_foreign_html_scope_barrier(*ns, tag, *integration) => {
                    return;
                }
                StackEntry::Foreign { .. } | StackEntry::XmlScript(_) => continue,
                StackEntry::Html(open) if open == target => {
                    self.stack.truncate(idx);
                    return;
                }
                StackEntry::Html(open)
                    if is_html_scope_barrier(open) || extra_barriers.contains(&open.as_slice()) =>
                {
                    return;
                }
                StackEntry::Html(_) => {}
            }
        }
    }

    fn pop_table_cells_to(&mut self, stops: &[&[u8]]) {
        let mut idx = self.stack.len();
        while idx > 0 {
            idx -= 1;
            match &self.stack[idx] {
                StackEntry::Html(open) if stops.contains(&open.as_slice()) => {
                    let has_table = open == b"table"
                        || self.stack[..=idx].iter().rev().any(|e| match e {
                            StackEntry::Html(t) if t == b"table" => true,
                            StackEntry::Html(t)
                                if matches!(t.as_slice(), b"template" | b"html") =>
                            {
                                false
                            }
                            _ => false,
                        });
                    if has_table && open != b"template" && open != b"html" {
                        self.stack.truncate(idx + 1);
                    }
                    return;
                }
                StackEntry::Foreign {
                    ns,
                    tag,
                    integration,
                } if is_foreign_html_scope_barrier(*ns, tag, *integration) => {
                    return;
                }
                _ => {}
            }
        }
    }

    pub(super) fn on_end_tag(&mut self, tag_name: &[u8]) {
        if self.xhtml {
            self.on_xhtml_end_tag(tag_name);
            return;
        }
        if matches!(self.stack.last(), Some(StackEntry::Foreign { .. })) {
            if self.in_foreign_content_point()
                && (tag_name.eq_ignore_ascii_case(b"p") || tag_name.eq_ignore_ascii_case(b"br"))
            {
                while self.in_foreign_content_point() {
                    self.stack.pop();
                }
                if tag_name.eq_ignore_ascii_case(b"p") && !self.in_select_scope() {
                    self.pop_html_element_in_scope(b"p");
                }
                return;
            }
            if tag_name.eq_ignore_ascii_case(b"script")
                && matches!(
                    self.stack.last(),
                    Some(StackEntry::Foreign { tag, .. }) if tag.as_slice() == b"script"
                )
            {
                self.stack.pop();
                return;
            }
            let mut idx = self.stack.len();
            while idx > 0 {
                idx -= 1;
                match &self.stack[idx] {
                    StackEntry::Foreign { tag, .. } => {
                        if tag.eq_ignore_ascii_case(tag_name) {
                            self.stack.truncate(idx);
                            return;
                        }
                    }
                    StackEntry::Html(_) => break,
                    StackEntry::XmlScript(_) => {}
                }
                if idx == 0 {
                    return;
                }
            }
        }
        if self.in_select_scope() {
            if tag_name.eq_ignore_ascii_case(b"select") {
                self.pop_select_in_scope();
            } else if is_table_scope_end_tag(tag_name) || tag_name.eq_ignore_ascii_case(b"template")
            {
                self.pop_html_element_in_scope(tag_name);
            }
            return;
        }
        self.pop_html_element_in_scope(tag_name);
    }

    fn on_xhtml_end_tag(&mut self, tag_name: &[u8]) {
        let local = super::xml::local_name(tag_name, true);
        let mut idx = self.stack.len();
        while idx > 0 {
            idx -= 1;
            let matched = match &self.stack[idx] {
                StackEntry::XmlScript(open) => {
                    local.eq_ignore_ascii_case(b"script")
                        && (open.eq_ignore_ascii_case(tag_name)
                            || super::xml::local_name(open, true).eq_ignore_ascii_case(b"script"))
                }
                StackEntry::Foreign { tag, .. } => tag.eq_ignore_ascii_case(local),
                StackEntry::Html(open) => {
                    open.eq_ignore_ascii_case(tag_name)
                        || super::xml::local_name(open, true).eq_ignore_ascii_case(local)
                }
            };
            if matched {
                self.stack.truncate(idx);
                return;
            }
        }
    }

    fn pop_html_element_in_scope(&mut self, tag_name: &[u8]) {
        let is_heading = is_heading_tag(tag_name);
        let use_table_scope = is_table_container_end_tag(tag_name);
        let mut idx = self.stack.len();
        while idx > 0 {
            idx -= 1;
            match &self.stack[idx] {
                StackEntry::XmlScript(_) => continue,
                StackEntry::Foreign {
                    ns,
                    tag,
                    integration,
                } => {
                    if !use_table_scope && is_foreign_html_scope_barrier(*ns, tag, *integration) {
                        return;
                    }
                }
                StackEntry::Html(open) => {
                    let matched = if is_heading {
                        is_heading_tag(open)
                    } else {
                        open.eq_ignore_ascii_case(tag_name)
                    };
                    if matched {
                        self.stack.truncate(idx);
                        return;
                    }
                    let barrier = if use_table_scope {
                        matches!(open.as_slice(), b"html" | b"table" | b"template")
                    } else {
                        is_html_scope_barrier(open)
                    };
                    if barrier {
                        return;
                    }
                }
            }
        }
    }
}

fn foreign_integration_kind(
    ns: ForeignNs,
    tag_name: &[u8],
    tag_attrs_raw: &str,
) -> IntegrationKind {
    match ns {
        ForeignNs::Svg => {
            if is_svg_html_integration_point(tag_name) {
                IntegrationKind::Html
            } else {
                IntegrationKind::None
            }
        }
        ForeignNs::Math => {
            if tag_name.eq_ignore_ascii_case(b"annotation-xml")
                && annotation_xml_is_html_integration(tag_attrs_raw)
            {
                IntegrationKind::Html
            } else if is_math_text_integration_point(tag_name) {
                IntegrationKind::MathText
            } else {
                IntegrationKind::None
            }
        }
    }
}

fn is_foreign_html_scope_barrier(
    ns: ForeignNs,
    lower_tag: &[u8],
    integration: IntegrationKind,
) -> bool {
    integration != IntegrationKind::None
        || (ns == ForeignNs::Math && lower_tag == b"annotation-xml")
}

fn closes_p_in_button_scope(lower_tag: &[u8]) -> bool {
    matches!(
        lower_tag,
        b"address"
            | b"article"
            | b"aside"
            | b"blockquote"
            | b"center"
            | b"details"
            | b"dialog"
            | b"dir"
            | b"div"
            | b"dl"
            | b"fieldset"
            | b"figcaption"
            | b"figure"
            | b"footer"
            | b"form"
            | b"h1"
            | b"h2"
            | b"h3"
            | b"h4"
            | b"h5"
            | b"h6"
            | b"header"
            | b"hgroup"
            | b"hr"
            | b"main"
            | b"menu"
            | b"nav"
            | b"ol"
            | b"p"
            | b"pre"
            | b"search"
            | b"section"
            | b"summary"
            | b"table"
            | b"ul"
    )
}

fn is_special_non_inline(lower_tag: &[u8]) -> bool {
    closes_p_in_button_scope(lower_tag)
        || matches!(
            lower_tag,
            b"button" | b"li" | b"tbody" | b"thead" | b"tfoot" | b"tr"
        )
}

fn is_svg_html_integration_point(tag: &[u8]) -> bool {
    tag.eq_ignore_ascii_case(b"foreignObject")
        || tag.eq_ignore_ascii_case(b"desc")
        || tag.eq_ignore_ascii_case(b"title")
}

fn is_math_text_integration_point(tag: &[u8]) -> bool {
    tag.eq_ignore_ascii_case(b"mi")
        || tag.eq_ignore_ascii_case(b"mo")
        || tag.eq_ignore_ascii_case(b"mn")
        || tag.eq_ignore_ascii_case(b"ms")
        || tag.eq_ignore_ascii_case(b"mtext")
}

fn is_html_void_element(tag: &[u8]) -> bool {
    const VOIDS: &[&[u8]] = &[
        b"area", b"base", b"br", b"col", b"embed", b"hr", b"img", b"input", b"link", b"meta",
        b"param", b"source", b"track", b"wbr",
    ];
    VOIDS.iter().any(|cand| tag.eq_ignore_ascii_case(cand))
}

fn is_heading_tag(tag: &[u8]) -> bool {
    tag.len() == 2 && matches!(tag[0], b'h' | b'H') && (b'1'..=b'6').contains(&tag[1])
}

fn is_table_container_end_tag(tag: &[u8]) -> bool {
    tag.eq_ignore_ascii_case(b"table")
        || tag.eq_ignore_ascii_case(b"tbody")
        || tag.eq_ignore_ascii_case(b"thead")
        || tag.eq_ignore_ascii_case(b"tfoot")
        || tag.eq_ignore_ascii_case(b"tr")
}

pub(super) fn is_table_scope_end_tag(tag: &[u8]) -> bool {
    is_table_container_end_tag(tag)
        || tag.eq_ignore_ascii_case(b"td")
        || tag.eq_ignore_ascii_case(b"th")
        || tag.eq_ignore_ascii_case(b"caption")
}

fn is_html_scope_barrier(lower_tag: &[u8]) -> bool {
    matches!(
        lower_tag,
        b"applet"
            | b"caption"
            | b"html"
            | b"marquee"
            | b"object"
            | b"table"
            | b"td"
            | b"template"
            | b"th"
    )
}

fn visit_simple_attrs<F: FnMut(&str, Option<&str>) -> bool>(tag_attrs_raw: &str, mut f: F) -> bool {
    let bytes = tag_attrs_raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b'/') {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] == b'>' {
            break;
        }
        let ns = i;
        if bytes[i] == b'=' {
            i += 1;
        }
        while i < bytes.len()
            && !bytes[i].is_ascii_whitespace()
            && !matches!(bytes[i], b'=' | b'>' | b'/')
        {
            i += 1;
        }
        let ne = i;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let val = if i < bytes.len() && bytes[i] == b'=' {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            let (vs, ve, next_i) = read_simple_attr_val(bytes, i);
            i = next_i;
            Some(&tag_attrs_raw[vs..ve])
        } else {
            None
        };
        if f(&tag_attrs_raw[ns..ne], val) {
            return true;
        }
    }
    false
}

fn font_has_breakout_attr(tag_attrs_raw: &str) -> bool {
    visit_simple_attrs(tag_attrs_raw, |name, _| {
        name.eq_ignore_ascii_case("color")
            || name.eq_ignore_ascii_case("face")
            || name.eq_ignore_ascii_case("size")
    })
}

fn annotation_xml_is_html_integration(tag_attrs_raw: &str) -> bool {
    let mut result = Option::<bool>::None;
    visit_simple_attrs(tag_attrs_raw, |name, val| {
        if name.eq_ignore_ascii_case("encoding") {
            let decoded = super::xml::decode_attr_entities(val.unwrap_or(""));
            result = Some(
                decoded.eq_ignore_ascii_case("text/html")
                    || decoded.eq_ignore_ascii_case("application/xhtml+xml"),
            );
            return true;
        }
        false
    });
    result.unwrap_or(false)
}

fn read_simple_attr_val(bytes: &[u8], pos: usize) -> (usize, usize, usize) {
    if pos >= bytes.len() || bytes[pos] == b'>' {
        return (pos, pos, pos);
    }
    let q = bytes[pos];
    if q == b'"' || q == b'\'' {
        let mut end = pos + 1;
        while end < bytes.len() && bytes[end] != q {
            end += 1;
        }
        let next = if end < bytes.len() { end + 1 } else { end };
        return (pos + 1, end, next);
    }
    let mut end = pos;
    while end < bytes.len() && !bytes[end].is_ascii_whitespace() && bytes[end] != b'>' {
        end += 1;
    }
    let val_end = if end > pos && bytes[end - 1] == b'/' && bytes.get(end) == Some(&b'>') {
        end - 1
    } else {
        end
    };
    (pos, val_end, end)
}

fn is_foreign_breakout_tag(tag: &[u8]) -> bool {
    const BREAKOUTS: &[&[u8]] = &[
        b"b",
        b"big",
        b"blockquote",
        b"body",
        b"br",
        b"center",
        b"code",
        b"dd",
        b"div",
        b"dl",
        b"dt",
        b"em",
        b"embed",
        b"h1",
        b"h2",
        b"h3",
        b"h4",
        b"h5",
        b"h6",
        b"head",
        b"hr",
        b"i",
        b"img",
        b"li",
        b"listing",
        b"menu",
        b"meta",
        b"nobr",
        b"ol",
        b"p",
        b"pre",
        b"ruby",
        b"s",
        b"small",
        b"span",
        b"strong",
        b"strike",
        b"sub",
        b"sup",
        b"table",
        b"tt",
        b"u",
        b"ul",
        b"var",
    ];
    BREAKOUTS.iter().any(|cand| tag.eq_ignore_ascii_case(cand))
}
