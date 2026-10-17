//! Compile a dictionary's stylesheet into inline markup (issue #57).
//!
//! The Kindle lookup popup does not apply `<style>` blocks. A dictionary can
//! ship a perfectly good stylesheet and every entry still renders in the
//! reader's default face, which is what every kindling dictionary did until
//! this module existed.
//!
//! kindlegen works around it by resolving the rules at build time into legacy
//! presentational tags. Given `u { font-size: 260%; font-weight: bold; }` it
//! writes `<u><font size="+3"><b>UNDER</b></font></u>` into the text and ships
//! no stylesheet at all. Device-confirmed on a Paperwhite: text carrying a
//! literal `<font size="+3">` renders large in the popup, and the identical
//! text styled by a rule does not.
//!
//! So this takes the same approach. These declarations are worth compiling,
//! because each has a legacy tag or attribute to become:
//!
//! | CSS | becomes |
//! |---|---|
//! | `font-size` | `<font size="+N">` |
//! | `color` | `<font color="…">` |
//! | `font-weight: bold` | `<b>` |
//! | `font-style: italic` | `<i>` |
//! | `text-decoration: underline` | `<u>` |
//! | `background-color` | `bgcolor="…"` on the element itself |
//! | `margin-left` | `<blockquote>` around the contents |
//! | `margin-top` (and `margin`) | `height="…"` on the element itself |
//! | `margin-bottom` (and `margin`) | a `<div height="…"></div>` after the element |
//! | `text-align` | `align="…"` on the element itself, only on `p`, `div`, `td`, `tr`, `blockquote` and `h1`–`h6` |
//!
//! Each mapping is one kindlegen was measured to make, nesting order included:
//! for a `<p>` carrying all of them kindlegen writes `<p height="20"
//! bgcolor="#eee"><blockquote><font size="+3" color="red"><b><i><u>…</u></i></b>
//! </font></blockquote></p><div height="20"></div>`, and so does this. A
//! `margin` shorthand feeds every side it names, and a side that rounds away
//! feeds nothing.
//!
//! The vertical margins are the crude half of the mapping, and worth knowing
//! about before using: a top margin becomes a `height` on the element rather
//! than space above it, and a bottom margin becomes an empty spacer element
//! dropped into the entry text. Both are what kindlegen writes, and both are
//! only approximations of a margin, so a value is rounded to a whole unit first
//! (see [`height_value`]) and vanishes when it rounds to nothing.
//!
//! Element *and* class selectors are compiled. [`apply`] runs on the entry's
//! source markup, before `strip_idx_markup` takes the `class` attributes out
//! of it, so a rule written against a class has its class to match — `.def`
//! reaches `<span class="def and more">` exactly as kindlegen resolves it. A
//! selector that needs a real cascade to evaluate it — an id, an attribute, a
//! pseudo-class, a combinator — is left in the sheet untouched, as is any
//! selector naming an element kindling does not know.
//!
//! Because both kinds of selector can match one element, they are combined the
//! way the cascade would: a class selector beats a bare element selector, more
//! classes beat fewer, and a selector that also names an element beats one that
//! does not. Rules of equal specificity keep source order, later winning.
//!
//! An element's own `style="…"` attribute is compiled too, and it is the most
//! specific declaration there is, so it is folded on last and wins over every
//! rule that matched. Without that, an entry that styles an element inline
//! loses the styling entirely, because `strip_idx_markup` takes `style`
//! attributes out of the shipped entries (issue #6: the library-search preview
//! bails on them) — the declarations reach neither the popup nor the sheet.
//! Measured against kindlegen 2.9, the attribute resolves to exactly the tags
//! and nesting the same declarations get from a rule: `style="font-size:260%"`
//! is `<font size="+3">`, `style="color:red"` is `<font color="red">`, and one
//! attribute holding all of them gives `<span height="20" bgcolor="#eee">
//! <blockquote><font size="+3" color="red"><b><i>…`, inline on the entry
//! wrapper itself included. A declaration it cannot compile — `padding-left: 2em`
//! — produces no markup and takes the attribute down with it, as kindlegen does,
//! so nothing about it reaches the reader either way. The one place this is
//! deliberately not kindlegen is the degenerate case: kindlegen writes a
//! `style` attribute that holds no declaration at all (`style=""`, `style=";"`)
//! back verbatim, and kindling strips it with every other attribute, which is
//! what the preview needs and what costs nothing to render.
//!
//! `text-align` becomes the element's own `align` attribute, but only on the
//! elements kindlegen writes one for. Measured one element at a time on
//! kindlegen 2.9: `p`, `div`, `td`, `tr`, `blockquote` and `h1`–`h6` carry the
//! attribute, while `th`, `li`, `table`, every inline element and an element
//! name no stylesheet knows (`<foo>`) get nothing. That is a whitelist, not
//! "anything that is not inline". The value is written through as it was
//! declared, the way a colour is (`center`, `CENTER`, `justify`, even `foo`),
//! and a trailing `!important` is dropped — kindlegen drops it from every
//! property, `color` and `font-size` included. An element that already carries
//! an `align` attribute keeps it: kindlegen does not write a second one, and
//! a second `align` is invalid markup anyway.
//!
//! Because which elements can carry the attribute is only known while `apply`
//! walks the markup, the residual sheet is decided more coarsely: a rule naming
//! an element outside the whitelist (`span`, `li`, `table`) keeps its
//! `text-align` declaration, since there is nothing it could compile to; a
//! class rule compiles, because the element that carries the class is not known
//! until it is matched. A class rule that ends up on a `p` gets the attribute,
//! and the same rule on a `span` gets nothing, which is exactly what kindlegen
//! does with one class and two elements.
//!
//! Declarations that are compiled are removed from the stylesheet that ships,
//! so a reader that *does* apply CSS cannot also apply the inline tag and
//! double the effect. Anything not compiled, `padding` and the like, stays in
//! the sheet untouched — including a rule that compiled nothing at all, which
//! is left byte-for-byte as the source wrote it.

use std::borrow::Cow;
use std::collections::HashMap;

/// The presentational effect a rule asks for, reduced to what the popup can
/// actually render.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Effect {
    /// Legacy relative font size, `-3` to `+3`. `None` leaves size alone.
    pub size: Option<i8>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    /// `color`, verbatim, for the `<font color="…">` face.
    pub color: Option<String>,
    /// `background-color`, verbatim, for the element's own `bgcolor`.
    pub background: Option<String>,
    /// A left margin, worth a legacy `<blockquote>` indent.
    pub indent: bool,
    /// A top margin, as the value of the element's own `height` attribute.
    pub height: Option<String>,
    /// A bottom margin, as the value of a spacer `<div>` written after the
    /// element.
    pub spacer: Option<String>,
    /// `text-align`, verbatim, for the element's own `align` attribute. Only
    /// some elements carry one; see [`carries_align`].
    pub align: Option<String>,
    /// An `unset` that takes a left margin away, so an indent an earlier rule
    /// asked for does not survive it. Measured: it is what keeps kindlegen from
    /// indenting the 21,890 `.subheadword` elements of the real Wadoku build,
    /// whose `.subheadword .sense { margin-left: unset }` follows the rule that
    /// asks for the indent.
    pub indent_unset: bool,
    /// The same for a top margin and the `height` attribute it becomes.
    pub height_unset: bool,
    /// The same for a bottom margin and the spacer `div` it becomes.
    pub spacer_unset: bool,
}

impl Effect {
    fn is_empty(&self) -> bool {
        self.size.is_none()
            && !self.bold
            && !self.italic
            && !self.underline
            && self.color.is_none()
            && self.background.is_none()
            && !self.indent
            && self.height.is_none()
            && self.spacer.is_none()
            && self.align.is_none()
    }

    /// Whether this asks for anything at all: content to write, or a margin an
    /// `unset` takes away.
    fn asks_for_something(&self) -> bool {
        !self.is_empty() || self.clears_a_margin()
    }

    /// Whether this takes a margin away, which is worth recording under a
    /// selector that already carries one and worth nothing under one that does
    /// not.
    fn clears_a_margin(&self) -> bool {
        self.indent_unset || self.height_unset || self.spacer_unset
    }

    /// Fold `other` over this, `other` winning.
    ///
    /// This is how two rules that both match an element are combined, so
    /// `other` is the more specific one (or the later of two equals): it
    /// replaces a size or a colour it declares and turns on the attributes it
    /// asks for, leaving alone everything it says nothing about.
    ///
    /// An `unset` is the one declaration that can turn a margin *off*, so it is
    /// the one that relaxes what an earlier rule asked for: `margin-left: unset`
    /// after `margin-left: 10px` leaves no indent, which is what kindlegen does
    /// with the real sheet's `.subheadword` and its `.subheadword .sense`.
    fn merge(&mut self, other: &Effect) {
        if other.size.is_some() {
            self.size = other.size;
        }
        if other.color.is_some() {
            self.color = other.color.clone();
        }
        if other.background.is_some() {
            self.background = other.background.clone();
        }
        if other.height.is_some() {
            self.height = other.height.clone();
        } else if other.height_unset {
            self.height = None;
        }
        if other.spacer.is_some() {
            self.spacer = other.spacer.clone();
        } else if other.spacer_unset {
            self.spacer = None;
        }
        if other.align.is_some() {
            self.align = other.align.clone();
        }
        self.bold |= other.bold;
        self.italic |= other.italic;
        self.underline |= other.underline;
        if other.indent {
            self.indent = true;
        } else if other.indent_unset {
            self.indent = false;
        }
    }

    /// The markup to open and close around an element's contents. Closing
    /// order mirrors opening order so the result nests properly.
    ///
    /// The order is kindlegen's, measured: `margin-left` becomes the
    /// outermost `<blockquote>`, then one `<font>` carrying size and colour,
    /// then `<b>`, `<i>`, `<u>`. Its own output for a `<p>` holding all of them
    /// is `<p bgcolor="#eee"><blockquote><font size="+3" color="red"><b><i><u>…`.
    fn markup(&self) -> (String, String) {
        let (mut open, mut close) = self.wrapping_markup();
        if self.indent {
            open.insert_str(0, "<blockquote>");
            close.push_str("</blockquote>");
        }
        (open, close)
    }

    /// Everything but the indent, which is outermost and belongs to the element
    /// rather than to its contents: one `<font>` carrying size and colour, then
    /// `<b>`, `<i>`, `<u>`.
    ///
    /// Split out for `<pre>`, whose indent wraps the generated wrapper instead
    /// of the contents, while these go inside it (see [`apply`]).
    fn wrapping_markup(&self) -> (String, String) {
        let mut open = String::new();
        let mut close = String::new();
        // One `<font>` carries both faces, `size` before `color`, which is how
        // kindlegen writes `font-size:200%;color:red`.
        let mut font = String::new();
        if let Some(n) = self.size {
            font.push_str(&format!(" size=\"{:+}\"", n));
        }
        if let Some(c) = &self.color {
            font.push_str(&format!(" color=\"{}\"", c));
        }
        if !font.is_empty() {
            open.push_str("<font");
            open.push_str(&font);
            open.push('>');
            close.insert_str(0, "</font>");
        }
        if self.bold {
            open.push_str("<b>");
            close.insert_str(0, "</b>");
        }
        if self.italic {
            open.push_str("<i>");
            close.insert_str(0, "</i>");
        }
        if self.underline {
            open.push_str("<u>");
            close.insert_str(0, "</u>");
        }
        (open, close)
    }

    /// The attributes to add to the element's own start tag, if any.
    ///
    /// Three declarations are not wrapped around the contents, because
    /// kindlegen puts them on the element itself: a top margin becomes
    /// `height`, `text-align` becomes `align`, and `background-color` becomes
    /// `bgcolor`. Measured, in that order however the declarations were written
    /// — `<p style="background-color:#eee;margin-top:20px;text-align:center">`
    /// and the reverse both ship as
    /// `<p height="20" align="center" bgcolor="#eee">` — so these have to
    /// land before the `>` that closes the start tag. The `bgcolor` half is
    /// also the reason the IDA work found the writer reading a colour out of
    /// its style struct in the branch that emits `bgcolor`.
    ///
    /// `align` needs two things the other two do not: the element has to be one
    /// kindlegen writes the attribute on ([`carries_align`]), and it must not
    /// already have an `align` of its own, which kindlegen leaves alone rather
    /// than overriding with the stylesheet's value.
    fn start_attributes(&self, element: &str, attrs: &str) -> Option<String> {
        let mut out = String::new();
        if let Some(h) = &self.height {
            out.push_str(&format!(" height=\"{}\"", h));
        }
        if let Some(a) = &self.align {
            if carries_align(element) && !has_attribute(attrs, "align") {
                out.push_str(&format!(" align=\"{}\"", a));
            }
        }
        if let Some(c) = &self.background {
            out.push_str(&format!(" bgcolor=\"{}\"", c));
        }
        (!out.is_empty()).then_some(out)
    }

    /// The spacer `kindlegen` writes after an element with a bottom margin.
    ///
    /// Measured: `<span style="margin-bottom:20px">` ships as `<span>…</span>
    /// <div height="20"></div>`, and the spacers nest with the elements, so an
    /// inner element's spacer lands inside its parent, before the parent's own
    /// closing tag.
    fn trailing_spacer(&self) -> Option<String> {
        self.spacer
            .as_ref()
            .map(|h| format!("<div height=\"{}\"></div>", h))
    }
}

/// A selector this can match against, reduced to the two things it may ask
/// for: an element name, and classes the element has to carry.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Selector {
    /// The element name the selector requires, as in `span.def`. `None` for a
    /// bare class selector.
    element: Option<String>,
    /// Every class the selector requires, as in `.def.more`. Empty for a bare
    /// element selector.
    classes: Vec<String>,
}

/// Parse one simple selector.
///
/// Returns `None` for anything that needs a real cascade to evaluate: an id, an
/// attribute selector, a pseudo-class, a combinator, the universal `*`, and
/// anything else the scanner does not recognise. A `\:` escape is the element
/// `idx:orth`'s colon, not a pseudo-class, and reaches the selector as a colon.
fn parse_selector(selector: &str) -> Option<Selector> {
    let s = selector.trim();
    if s.is_empty() {
        return None;
    }
    let mut element: Option<String> = None;
    let mut classes: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut seen_dot = false;
    let mut ends_on_dot = false;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            // CSS escaping: the next character is literal, whatever it is.
            '\\' => {
                cur.push(chars.next()?);
                ends_on_dot = false;
            }
            '.' => {
                if seen_dot {
                    // Ends the class being built; `..` would end an empty one.
                    if cur.is_empty() {
                        return None;
                    }
                    classes.push(std::mem::take(&mut cur));
                } else if cur.is_empty() {
                    // A leading dot: this is a class-only selector.
                } else {
                    element = Some(std::mem::take(&mut cur));
                }
                seen_dot = true;
                ends_on_dot = true;
            }
            c if c.is_ascii_alphanumeric() || c == '-' || c == '_' => {
                cur.push(c);
                ends_on_dot = false;
            }
            // An unescaped `:`, `#`, `[`, `*`, whitespace, `>`, `+`, `~`.
            _ => return None,
        }
    }
    if ends_on_dot {
        // `span.` and `.a.` name an empty class; a lone `.` names nothing.
        return None;
    }
    if !cur.is_empty() {
        if seen_dot {
            classes.push(cur);
        } else {
            element = Some(cur);
        }
    }

    if let Some(name) = &element {
        if !is_element_name(name) {
            return None;
        }
    }
    if element.is_none() && classes.is_empty() {
        return None;
    }
    if classes.iter().any(|c| !is_class_name(c)) {
        return None;
    }

    Some(Selector {
        element: element.map(|e| e.to_ascii_lowercase()),
        classes: classes
            .into_iter()
            .map(|c| c.to_ascii_lowercase())
            .collect(),
    })
}

/// What one part of a selector list asks of the markup.
#[derive(Debug)]
enum Part {
    /// A simple selector, matched as written.
    Simple(Selector),
    /// A descendant selector, compiled against its leading compound. Measured
    /// on kindlegen 2.9: `.a .b` indents a `.a` even when the entry holds no
    /// `.b` at all, `span .b` marks every `span`, and `div.a .b` reaches a
    /// `div.a` — so the compound before the space is what the rule acts on, and
    /// the elements the descendant half names are left to the sheet.
    Leading(Selector),
    /// A selector nothing here can read, not even a leading compound: an id, an
    /// attribute, a pseudo-class, the universal selector, or a combinator other
    /// than the descendant one. Measured: `.a + .b`, `.a > .b` and `a.ref > *`
    /// all produce nothing at all in kindlegen, so a child or sibling
    /// combinator is not read as its leading compound the way a space is.
    Unreadable,
}

/// Read one part of a selector list.
fn read_part(part: &str) -> Part {
    let text = without_comments(part);
    if let Some(selector) = parse_selector(&text) {
        return Part::Simple(selector);
    }
    match leading_compound(&text) {
        Some(lead) => Part::Leading(lead),
        None => Part::Unreadable,
    }
}

/// The leading compound of a descendant selector, when it is a simple selector
/// this can match.
///
/// It is everything before the first whitespace, and `None` comes back when that
/// compound itself carries something unsupported — `#x .b` names an id — or when
/// the whitespace is not a descendant combinator but the start of `>`, `+` or
/// `~`. A compound of several classes reads as one selector here, since this
/// matches what a document says (`.a.master` against `class="a master"`), even
/// though kindlegen 2.9 was measured not to match such a compound at all; that
/// only ever compiles rules kindlegen leaves alone.
fn leading_compound(text: &str) -> Option<Selector> {
    let text = text.trim();
    let at = text.find(char::is_whitespace)?;
    let lead = &text[..at];
    let rest = text[at..].trim_start();
    if lead.ends_with(['>', '+', '~']) || rest.starts_with(['>', '+', '~']) {
        return None;
    }
    parse_selector(lead)
}

/// True for a name the tag scanner could report as an element: a letter
/// followed by letters, digits, `-`, `_` or the colon of `idx:orth`.
fn is_element_name(name: &str) -> bool {
    name.starts_with(|c: char| c.is_ascii_alphabetic())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == ':')
}

/// True for a name usable as a class token. No leading-letter rule: a document
/// may legitimately write `class="2col"`, and a rule for it should compile.
fn is_class_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// The elements kindlegen writes a legacy `align` attribute on.
///
/// Measured one element at a time on kindlegen 2.9, and it is a short list:
/// `p`, `div`, `td`, `tr`, `blockquote` and `h1`–`h6`. Every other element was
/// left alone, including the ones that are also block-level in CSS (`li`,
/// `table`, `th`, `section`, `pre`) and, of course, the inline ones. An element
/// name no stylesheet knows (`<foo>`) gets nothing either, which is what makes
/// this a whitelist rather than a test for "not inline".
///
/// `element` is the name `for_each_tag` reports, which is already lowercased,
/// and every selector that reaches here is lowercased by `parse_selector`.
fn carries_align(element: &str) -> bool {
    matches!(element, "p" | "div" | "td" | "tr" | "blockquote")
        || matches!(element.as_bytes(), [b'h', b'1'..=b'6'])
}

/// A rule keyed by a class selector, kept in source order.
struct ClassRule {
    /// The element the selector requires, as in `span.def`.
    element: Option<String>,
    /// Every class the selector requires, all of which must be present.
    classes: Vec<String>,
    effect: Effect,
}

/// Everything a stylesheet compiled to.
///
/// Element rules go in a map because every element is looked up against them;
/// class rules stay a list because only an element that actually carries a
/// `class` attribute is tested against them, and there are few.
#[derive(Default)]
pub(crate) struct Effects {
    elements: HashMap<String, Effect>,
    classes: Vec<ClassRule>,
}

impl Effects {
    fn is_empty(&self) -> bool {
        self.elements.is_empty() && self.classes.is_empty()
    }

    /// Whether a rule is already recorded under this selector.
    fn holds(&self, selector: &Selector) -> bool {
        match &selector.element {
            Some(name) if selector.classes.is_empty() => self.elements.contains_key(name),
            _ => self
                .classes
                .iter()
                .any(|r| r.element == selector.element && r.classes == selector.classes),
        }
    }

    /// Record one compiled rule under the selector that asked for it.
    fn insert(&mut self, selector: &Selector, effect: &Effect) {
        // A rule that only clears a margin is worth recording under a selector
        // that already has one, and worth no entry at all under a selector that
        // has none: there is no margin to take away. Skipping it keeps a sheet
        // of pure `unset`s from costing the entry walk.
        if effect.is_empty() && !self.holds(selector) {
            return;
        }
        match &selector.element {
            Some(name) if selector.classes.is_empty() => {
                self.elements
                    .entry(name.clone())
                    .and_modify(|e| e.merge(effect))
                    .or_insert_with(|| effect.clone());
            }
            _ => {
                if let Some(existing) = self
                    .classes
                    .iter_mut()
                    .find(|r| r.element == selector.element && r.classes == selector.classes)
                {
                    existing.effect.merge(effect);
                } else {
                    self.classes.push(ClassRule {
                        element: selector.element.clone(),
                        classes: selector.classes.clone(),
                        effect: effect.clone(),
                    });
                }
            }
        }
    }

    /// Fold another stylesheet's rules into this one, later winning.
    fn merge(&mut self, other: Effects) {
        for (name, effect) in other.elements {
            self.elements
                .entry(name)
                .and_modify(|e| e.merge(&effect))
                .or_insert(effect);
        }
        for rule in other.classes {
            if let Some(existing) = self
                .classes
                .iter_mut()
                .find(|r| r.element == rule.element && r.classes == rule.classes)
            {
                existing.effect.merge(&rule.effect);
            } else {
                self.classes.push(rule);
            }
        }
    }

    /// The effect an element should carry, if any rule matches it or its own
    /// `style` attribute asks for one.
    ///
    /// `attrs` is the element's raw attribute text. It is only parsed when a
    /// class rule exists or a `style` attribute might, so a stylesheet of
    /// element selectors alone over markup with no inline styles still costs
    /// nothing extra on the way through a dictionary's half a million entries.
    fn get(&self, element: &str, attrs: &str) -> Option<Effect> {
        let mut effect = self.elements.get(element).cloned();

        if self.classes.is_empty() && !might_carry_style(attrs) {
            return effect;
        }
        let (classes, style) = attribute_values(attrs);

        if !self.classes.is_empty() && !classes.is_empty() {
            // Everything that matches, weakest first: a class selector beats a
            // bare element selector, and one requiring more classes beats one
            // requiring fewer. Source order decides between equal specificity,
            // which a stable sort preserves.
            let mut matched: Vec<&ClassRule> = self
                .classes
                .iter()
                .filter(|r| {
                    r.element.as_deref().is_none_or(|e| e == element)
                        && r.classes.iter().all(|c| classes.contains(c))
                })
                .collect();
            matched.sort_by_key(|r| (r.classes.len(), r.element.is_some()));

            for rule in matched {
                match &mut effect {
                    Some(e) => e.merge(&rule.effect),
                    None => effect = Some(rule.effect.clone()),
                }
            }
        }

        // The element's own `style` attribute is the most specific declaration
        // there is, so it goes on last and wins over everything that matched.
        // Measured: kindlegen resolves an inline `font-size: 260%` into
        // `<font size="+3">` even when a rule for the same element asked for
        // 150%, and an inline `font-weight: bold` beside an element's own
        // `color` rule gives `<font color="…"><b>`.
        if let Some(style) = style {
            // An inline declaration has no sheet to fall back into, so whether
            // the element could carry an `align` is not a reason to refuse it;
            // `start_attributes` decides what actually ships.
            let (own, _) = compile_declarations(style, true);
            if own.asks_for_something() {
                match &mut effect {
                    Some(e) => e.merge(&own),
                    None => effect = Some(own),
                }
            }
        }
        effect
    }
}

/// The two attribute values the compiler can read, taken in one pass over an
/// element's attribute list: the `class` tokens, lowercased, and the raw value
/// of an element's own `style` attribute.
///
/// This is a scanner rather than a search for `class=` because that string
/// turns up inside other attributes' values; it walks the attribute list the
/// way a parser would, skipping each value whole.
///
/// Class names are compared case-insensitively, unlike CSS, where the match is
/// case-sensitive. Element names here are folded the same way, and a sheet
/// written as `.Def` against markup that says `class="Def"` agrees either way;
/// folding only stops a sheet and its markup that disagree in case from
/// silently doing nothing. Attribute *names* are folded for the same reason: a
/// document may spell either one `STYLE=`. A `style` attribute written twice
/// keeps the first, which is not worth a rule because the markup is malformed
/// either way.
fn attribute_values(attrs: &str) -> (Vec<String>, Option<&str>) {
    let bytes = attrs.as_bytes();
    let mut out: Vec<String> = Vec::new();
    let mut style: Option<&str> = None;
    let mut i = 0usize;
    while i < bytes.len() {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b'/') {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let name_start = i;
        while i < bytes.len()
            && !bytes[i].is_ascii_whitespace()
            && bytes[i] != b'='
            && bytes[i] != b'/'
        {
            i += 1;
        }
        let name = &attrs[name_start..i];
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'=' {
            // A valueless attribute; there is nothing to read.
            continue;
        }
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let (value_start, value_end) = if i < bytes.len() && (bytes[i] == b'"' || bytes[i] == b'\'')
        {
            let quote = bytes[i];
            i += 1;
            let start = i;
            while i < bytes.len() && bytes[i] != quote {
                i += 1;
            }
            let end = i;
            if i < bytes.len() {
                i += 1;
            }
            (start, end)
        } else {
            let start = i;
            while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            (start, i)
        };
        if name.eq_ignore_ascii_case("class") {
            out.extend(
                attrs[value_start..value_end]
                    .split_whitespace()
                    .map(|t| t.to_ascii_lowercase()),
            );
        } else if name.eq_ignore_ascii_case("style") && style.is_none() {
            style = Some(&attrs[value_start..value_end]);
        }
    }
    (out, style)
}

/// Whether an attribute list could hold a `style` attribute.
///
/// A case-insensitive substring test, used to skip the attribute walk for the
/// element that has no `style` at all, which is nearly all of them. It scans
/// bytes rather than allocating a lowercased copy, and cannot reject a real
/// attribute: an attribute name is written literally, with no escapes and no
/// entities, so a `style` attribute always contains those five bytes in some
/// case.
fn might_carry_style(attrs: &str) -> bool {
    attrs
        .as_bytes()
        .windows(5)
        .any(|w| w.eq_ignore_ascii_case(b"style"))
}

/// Whether an attribute list already has an attribute by this name.
///
/// Walks the list the way [`attribute_values`] does — name, then the value it
/// may be followed by, skipped whole — so `data-align="x"` and `title='align'`
/// are not mistaken for the attribute itself. Only reached when an effect wants
/// to write `align`, so the extra walk costs nothing on the entries that do not
/// ask for one.
fn has_attribute(attrs: &str, wanted: &str) -> bool {
    let bytes = attrs.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b'/') {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let name_start = i;
        while i < bytes.len()
            && !bytes[i].is_ascii_whitespace()
            && bytes[i] != b'='
            && bytes[i] != b'/'
        {
            i += 1;
        }
        let name = &attrs[name_start..i];
        if name.eq_ignore_ascii_case(wanted) {
            return true;
        }
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'=' {
            // A valueless attribute; the next iteration reads the next name.
            continue;
        }
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i < bytes.len() && (bytes[i] == b'"' || bytes[i] == b'\'') {
            let quote = bytes[i];
            i += 1;
            while i < bytes.len() && bytes[i] != quote {
                i += 1;
            }
            if i < bytes.len() {
                i += 1;
            }
        } else {
            while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                i += 1;
            }
        }
    }
    false
}

/// What `compile` produced: the effects to inline, and the stylesheet left
/// over once the compiled declarations are taken out of it.
pub(crate) struct Compiled {
    pub effects: Effects,
    pub residual_css: String,
}

/// Map a CSS font-size to a legacy relative size.
///
/// Legacy sizes run 1 to 7 with 3 as the base, so the useful range either way
/// is three steps. Only ratios are handled: a percentage or an `em` is
/// relative to the surrounding text and can be converted, while `12pt` cannot,
/// because nothing here knows what the reader's base size is. The thresholds
/// put 260% at `+3`, which is what kindlegen emits for it.
fn size_delta(value: &str) -> Option<i8> {
    let v = value.trim().to_ascii_lowercase();
    let ratio = if let Some(pct) = v.strip_suffix('%') {
        pct.trim().parse::<f32>().ok()? / 100.0
    } else if let Some(em) = v.strip_suffix("rem").or_else(|| v.strip_suffix("em")) {
        em.trim().parse::<f32>().ok()?
    } else {
        match v.as_str() {
            "xx-large" => 2.0,
            "x-large" => 1.5,
            "large" | "larger" => 1.2,
            "small" | "smaller" => 0.83,
            "x-small" => 0.7,
            "xx-small" => 0.5,
            "medium" => 1.0,
            _ => return None,
        }
    };
    Some(match ratio {
        r if r >= 2.0 => 3,
        r if r >= 1.5 => 2,
        r if r >= 1.15 => 1,
        r if r <= 0.5 => -3,
        r if r <= 0.7 => -2,
        r if r <= 0.85 => -1,
        _ => return None,
    })
}

/// Split a declaration block into `(property, value)` pairs, keeping the
/// original text of each so uncompiled ones can be put back verbatim.
///
/// The property and value are read from the declaration with its comments taken
/// out, so `/* why */ color: red` is a `color` and `red /* and not pink */` is
/// `red`; the original text is what a declaration that is not compiled puts
/// back. A declaration that is nothing but a comment has no property to read and
/// is kept for the sheet rather than dropped, since a rule that is written back
/// should carry what its author wrote in it.
fn declarations(block: &str) -> Vec<(String, String, String)> {
    block
        .split(';')
        .filter(|d| !d.trim().is_empty())
        .filter_map(|d| {
            let bare = without_comments(d);
            if bare.trim().is_empty() {
                return Some((String::new(), String::new(), d.to_string()));
            }
            let (name, value) = bare.split_once(':')?;
            Some((
                name.trim().to_ascii_lowercase(),
                without_important(value.trim()).to_string(),
                d.to_string(),
            ))
        })
        .collect()
}

/// Drop a trailing `!important` from a declaration's value.
///
/// Measured: kindlegen drops it from every property it compiles —
/// `color:red !important` ships as `<font color="red">` and
/// `text-align:center !important` as `align="center"` — so this is done once
/// for the whole declaration list rather than in one property's branch. The
/// original text of an uncompiled declaration, `!important` included, is what
/// goes back into the sheet.
fn without_important(value: &str) -> &str {
    let trimmed = value.trim_end();
    let Some(bang) = trimmed.rfind('!') else {
        return trimmed;
    };
    if trimmed[bang + 1..].trim().eq_ignore_ascii_case("important") {
        trimmed[..bang].trim_end()
    } else {
        trimmed
    }
}

/// Split `selectors { body }` apart, counting braces rather than scanning for
/// the first `}`.
///
/// An at-rule can hold rules of its own, so the brace that closes `body` is not
/// necessarily the first one: `@media screen { p { color: red } }` has two. The
/// body and everything after the matching brace come back, or `None` when the
/// brace is never closed.
fn matching_block(s: &str) -> Option<(&str, &str)> {
    let mut depth = 0usize;
    for (i, c) in s.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some((&s[1..i], &s[i + 1..]));
                }
            }
            _ => {}
        }
    }
    None
}

/// A declaration's value, ready to sit inside an attribute.
///
/// kindlegen passes the value through untranslated — measured: `red`, `#ff0000`
/// and even `rgb(255,0,0)` and `transparent` all reach the shipped text as
/// written, and so do `text-align`'s `center`, `CENTER`, `justify` and `foo` —
/// so nothing is normalised or validated here.
///
/// A value carrying a quote or an angle bracket is refused instead, because it
/// would close the attribute and let the rest of the declaration escape into
/// the markup.
fn attribute_value(value: &str) -> Option<String> {
    let v = value.trim();
    if v.is_empty() || v.contains(['"', '\'', '<', '>']) {
        return None;
    }
    Some(v.to_string())
}

/// Map one CSS length to the `height` value kindlegen writes for it.
///
/// Measured, and this is kindlegen's table rather than CSS's. A number with no
/// unit, and the absolute units `px`, `pt`, `in`, `cm`, `mm` and `pc`, are
/// converted to whole pixels and written bare (`12pt` becomes `16`); `rem` is
/// written bare too, as though it were a pixel value; `em` keeps its unit after
/// rounding, and `ex` is halved into one (`2ex` becomes `1em`); a percentage
/// keeps its sign (`5%` stays `5%`); and `ch`, `vw`, `vh` and anything else
/// produce nothing at all.
///
/// Rounding is half away from zero, so `1.5px` is `2` and `2.5px` is `3`, and
/// the result decides whether anything is emitted: at `0.3em` and `0.4em`
/// nothing is, and at `0.5em` it becomes `1em`. That single test is what the
/// `<blockquote>` for a left margin uses too, which is why a margin of `0.4em`
/// indents nothing even though the value is not zero.
///
/// `None` also covers a value that cannot be read — `auto`, `inherit`,
/// `calc(…)`, `var(…)` — and a negative length, both treated as no height
/// rather than guessed at.
fn height_value(value: &str) -> Option<String> {
    let value = value.trim();
    if value.starts_with('-') {
        return None;
    }
    let digits_end = value
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(value.len());
    let (number, unit) = value.split_at(digits_end);
    let number: f32 = number.parse().ok()?;

    match unit.trim().to_ascii_lowercase().as_str() {
        // Whole units, written without one.
        "" | "px" | "rem" => rounded(number, ""),
        "pt" => rounded(number * 4.0 / 3.0, ""),
        "in" => rounded(number * 96.0, ""),
        "cm" => rounded(number * 96.0 / 2.54, ""),
        "mm" => rounded(number * 96.0 / 25.4, ""),
        "pc" => rounded(number * 16.0, ""),
        // Relative, so each keeps a unit: two `ex` are one `em`.
        "em" => rounded(number, "em"),
        "ex" => rounded(number / 2.0, "em"),
        "%" => rounded(number, "%"),
        _ => None,
    }
}

/// Round half away from zero, and drop a result that rounded to nothing.
fn rounded(number: f32, unit: &str) -> Option<String> {
    let whole = (number + 0.5).floor();
    if whole < 1.0 {
        return None;
    }
    Some(format!("{}{}", whole as i64, unit))
}

/// The top, bottom and left components of a `margin` shorthand.
///
/// CSS gives one value for all four sides, two for vertical/horizontal, three
/// for top/horizontal/bottom and four for top/right/bottom/left. Right is the
/// only side kindlegen has no legacy form for, so it is not returned.
fn margin_sides(value: &str) -> (Option<&str>, Option<&str>, Option<&str>) {
    let parts: Vec<&str> = value.split_whitespace().collect();
    match parts.len() {
        1 => (Some(parts[0]), Some(parts[0]), Some(parts[0])),
        2 => (Some(parts[0]), Some(parts[0]), Some(parts[1])),
        3 => (Some(parts[0]), Some(parts[2]), Some(parts[1])),
        4 => (Some(parts[0]), Some(parts[2]), Some(parts[3])),
        _ => (None, None, None),
    }
}

/// Whether a declaration's value is the CSS keyword that takes the property
/// away rather than asking for it.
///
/// `unset` is the one that matters, because it is the one the real sheet uses —
/// `.subheadword .sense { margin-left: unset }` — and the one kindlegen was
/// measured to honour, leaving the 21,890 `.subheadword` elements of the real
/// Wadoku build unindented where kindling kept the indent its other rule asked
/// for. Only `unset` is read this way: a value that is merely not a length —
/// `auto`, `0`, `0.4em`, `calc(…)` — is left alone rather than taken for a
/// reset, and stays in the sheet where it always did.
fn unsets(value: &str) -> bool {
    value.trim().eq_ignore_ascii_case("unset")
}

/// Compile one declaration list — the body of a rule, or the value of an
/// element's own `style` attribute — into the effect it asks for, plus the
/// text of every declaration it could not compile.
///
/// The two callers differ only in what they do with the leftovers: a rule puts
/// them back into the stylesheet it ships, while an inline style has no sheet
/// to put them in and drops them with the attribute. Measured, kindlegen does
/// the same: an element whose style holds only `padding-left: 2em` ships with
/// no attribute and no markup, so nothing about that declaration reaches the
/// reader either way.
///
/// `align_expressible` says whether the declarations belong to something that
/// can become an `align` attribute — a rule naming an element kindlegen aligns,
/// or a class rule, whose element is not known until it is matched. When it is
/// false, `text-align` is left in the leftovers rather than silently dropped.
fn compile_declarations(block: &str, align_expressible: bool) -> (Effect, Vec<String>) {
    let mut effect = Effect::default();
    let mut kept: Vec<String> = Vec::new();
    for (prop, value, original) in declarations(block) {
        let mut consumed = true;
        match prop.as_str() {
            // Only an element kindlegen writes `align` on can express this.
            // A rule for one that cannot keeps the declaration in the sheet,
            // where the book view still applies it; a class rule passes `true`
            // because the element it lands on is not known yet.
            "text-align" if align_expressible => match attribute_value(&value) {
                Some(a) => effect.align = Some(a),
                None => consumed = false,
            },
            "font-size" => match size_delta(&value) {
                Some(d) => effect.size = Some(d),
                None => consumed = false,
            },
            "font-weight" => {
                let bold = value == "bold"
                    || value == "bolder"
                    || value.parse::<u32>().map(|n| n >= 600).unwrap_or(false);
                if bold {
                    effect.bold = true;
                } else {
                    consumed = false;
                }
            }
            "font-style" => {
                if value == "italic" || value == "oblique" {
                    effect.italic = true;
                } else {
                    consumed = false;
                }
            }
            "text-decoration" | "text-decoration-line" => {
                if value.split_whitespace().any(|w| w == "underline") {
                    effect.underline = true;
                } else {
                    consumed = false;
                }
            }
            "color" => match attribute_value(&value) {
                Some(c) => effect.color = Some(c),
                None => consumed = false,
            },
            "background-color" => match attribute_value(&value) {
                Some(c) => effect.background = Some(c),
                None => consumed = false,
            },
            "margin-top" if unsets(&value) => {
                effect.height = None;
                effect.height_unset = true;
                consumed = false;
            }
            "margin-top" => match height_value(&value) {
                Some(h) => effect.height = Some(h),
                None => consumed = false,
            },
            "margin-bottom" if unsets(&value) => {
                effect.spacer = None;
                effect.spacer_unset = true;
                consumed = false;
            }
            "margin-bottom" => match height_value(&value) {
                Some(h) => effect.spacer = Some(h),
                None => consumed = false,
            },
            "margin-left" if unsets(&value) => {
                effect.indent = false;
                effect.indent_unset = true;
                consumed = false;
            }
            "margin-left" => {
                if height_value(&value).is_some() {
                    effect.indent = true;
                } else {
                    consumed = false;
                }
            }
            // All three vertical and horizontal sides come out of the one
            // declaration, so it is consumed if any of them was expressed.
            "margin" => {
                let (top, bottom, left) = margin_sides(&value);
                let mut expressed = false;
                if unsets(&value) {
                    // `margin: unset` is every side at once, so it clears every
                    // margin an earlier rule asked for.
                    effect.height = None;
                    effect.height_unset = true;
                    effect.spacer = None;
                    effect.spacer_unset = true;
                    effect.indent = false;
                    effect.indent_unset = true;
                }
                if let Some(h) = top.and_then(height_value) {
                    effect.height = Some(h);
                    effect.height_unset = false;
                    expressed = true;
                }
                if let Some(h) = bottom.and_then(height_value) {
                    effect.spacer = Some(h);
                    effect.spacer_unset = false;
                    expressed = true;
                }
                if left.and_then(height_value).is_some() {
                    effect.indent = true;
                    effect.indent_unset = false;
                    expressed = true;
                }
                if !expressed {
                    consumed = false;
                }
            }
            _ => consumed = false,
        }
        if !consumed {
            kept.push(original);
        }
    }
    (effect, kept)
}

/// The selector text with CSS comments taken out.
///
/// A comment is not a selector, but it stands exactly where one is expected:
/// the real Wadoku sheet opens with a `/** … */` banner before `.poses`, and
/// since the text between two rules is read as the next rule's selector list,
/// that banner landed inside it and disqualified the rule behind it. The whole
/// sheet then compiled for nothing but its first rule. Anything from `/*` to the
/// next `*/` goes, and an unterminated `/*` swallows the rest of the text, which
/// is what a browser does with it too.
///
/// Only the parse sees this: whatever a rule leaves in the sheet is written back
/// as the author wrote it, comments included.
fn without_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        match rest[start..].find("*/") {
            Some(end) => rest = &rest[start + end + 2..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// Compile a stylesheet into inline effects plus whatever is left of it.
///
/// Later rules win over earlier ones for the same selector, which is the
/// cascade's behavior for equal specificity.
pub(crate) fn compile(css: &str) -> Compiled {
    let mut effects = Effects::default();
    let mut residual = String::new();
    let mut rest = css;

    while !rest.is_empty() {
        let Some(open) = rest.find('{') else {
            // Trailing whitespace, a comment, a stray fragment. Keep it all.
            residual.push_str(rest);
            break;
        };
        let Some((block, after)) = matching_block(&rest[open..]) else {
            // An unclosed brace; there is no rule to read here.
            residual.push_str(rest);
            break;
        };
        let selectors = &rest[..open];
        // The rule exactly as the source wrote it, braces included.
        let whole = &rest[..rest.len() - after.len()];
        rest = after;

        // An at-rule brings its own nesting and cascade; leave it whole.
        if selectors.trim_start().starts_with('@') {
            residual.push_str(whole);
            continue;
        }

        // Every part that can be read is compiled, and the rule stays in the
        // sheet for the parts that cannot — a descendant selector, an id, a
        // pseudo-class — carrying its whole declaration block there, because
        // only a stylesheet can reach the elements those parts name.
        //
        // Measured on the real Wadoku sheet: one `.season small` in a list
        // beside five usable selectors cost the build 82,716 of the 107,416
        // `<font>` tags kindlegen 2.9 writes, since dropping the rule dropped
        // `rt`, `.accent`, `i.genus`, `.descr` and `.etym` with it.
        let parts: Vec<&str> = selectors.split(',').collect();
        let mut parsed: Vec<Selector> = Vec::new();
        // The parts that were compiled and can carry the declarations this left
        // uncompiled, and the parts the sheet has to keep the whole block for.
        let mut readable: Vec<&str> = Vec::new();
        let mut sheet_parts: Vec<&str> = Vec::new();
        for part in &parts {
            match read_part(part) {
                Part::Simple(selector) => {
                    parsed.push(selector);
                    readable.push(part);
                }
                // A descendant selector is compiled against its leading
                // compound, but the rule still has to reach the sheet: the
                // elements its descendant half names can only be styled from
                // there, and the compiled effect lands on different ones.
                Part::Leading(selector) => {
                    parsed.push(selector);
                    sheet_parts.push(part);
                }
                Part::Unreadable => sheet_parts.push(part),
            }
        }
        if parsed.is_empty() {
            // Nothing in the rule can be read, so the sheet keeps it whole.
            residual.push_str(whole);
            continue;
        }

        // A selector that names an element cannot be any other element, so the
        // whole rule's `text-align` is either expressible or is not. A class
        // selector does not name one, so it may land on a `p` and has to
        // compile. A selector list counts if any part can carry `align`.
        let align_expressible = parsed
            .iter()
            .any(|s| s.element.as_deref().is_none_or(carries_align));
        let (effect, kept) = compile_declarations(block, align_expressible);

        if effect.asks_for_something() {
            for selector in &parsed {
                effects.insert(selector, &effect);
            }
        }
        // The parts that could not be read keep the rule's declarations, whole
        // block and all: nothing else can style the elements they name. The
        // parts that were compiled have theirs removed below, so no element is
        // ever reachable by both a compiled tag and the sheet's own rule.
        if !sheet_parts.is_empty() && !effect.is_empty() {
            residual.push_str(&sheet_parts.join(","));
            residual.push('{');
            residual.push_str(block);
            residual.push_str(";}");
        }
        // Put the rest back only if something in it was not compiled. A rule
        // that compiled nothing is written back exactly as it arrived rather
        // than rebuilt, so a sheet this cannot use still reaches the reader
        // unchanged down to its spacing.
        if !kept.is_empty() {
            if effect.is_empty() {
                residual.push_str(whole);
            } else {
                residual.push_str(&readable.join(","));
                residual.push('{');
                residual.push_str(&kept.join(";"));
                residual.push_str(";}");
            }
        }
    }

    Compiled {
        effects,
        residual_css: residual,
    }
}

/// Compile the `<style>` blocks kindling collected from a dictionary's source.
///
/// Takes the blocks concatenated as they were gathered and gives back the
/// effects to inline plus the blocks that still have something in them. A
/// block whose every declaration was compiled is dropped, because an empty
/// stylesheet is worth no bytes.
pub(crate) fn compile_style_blocks(blocks: &str) -> Compiled {
    let mut effects = Effects::default();
    let mut residual = String::new();
    let lower = blocks.to_ascii_lowercase();
    let mut cursor = 0usize;

    while let Some(rel) = lower[cursor..].find("<style") {
        let tag_start = cursor + rel;
        // Anything between two blocks is not CSS; carry it through untouched.
        residual.push_str(&blocks[cursor..tag_start]);
        let Some(rel_open_end) = lower[tag_start..].find('>') else {
            break;
        };
        let open_end = tag_start + rel_open_end + 1;
        let Some(rel_close) = lower[open_end..].find("</style>") else {
            break;
        };
        let close = open_end + rel_close;

        let compiled = compile(&blocks[open_end..close]);
        effects.merge(compiled.effects);
        if !compiled.residual_css.trim().is_empty() {
            residual.push_str(&blocks[tag_start..open_end]);
            residual.push_str(&compiled.residual_css);
            residual.push_str("</style>");
        }
        cursor = close + "</style>".len();
    }
    residual.push_str(&blocks[cursor..]);

    Compiled {
        effects,
        residual_css: residual,
    }
}

/// Wrap the contents of every matching element in the markup its rules ask
/// for.
///
/// An element's own `style` attribute is read in the same pass, so a dictionary
/// whose styling is entirely inline is compiled even with no stylesheet at all.
///
/// Insertions go inside the element rather than around it, so an element that
/// carries an id stays where a link expects to find it. Nothing else in the
/// markup moves, which matters because dictionary entry offsets are measured
/// on these bytes afterwards.
///
/// The exception is `<pre>`, which kindlegen does not ship at all: it is
/// replaced by a spacer, the indent, `<div align="left"><tt>` and
/// `</tt></div>`, with its whitespace spelled out first (see
/// [`encode_pre_text`]).
pub(crate) fn apply<'a>(html: &'a str, effects: &Effects) -> Cow<'a, str> {
    // A stylesheet with nothing in it still has the entries' own `style`
    // attributes to compile, so the scan for one is what decides whether there
    // is any work at all. A `<pre>` is work on its own: kindlegen transliterates
    // it whether or not any rule reaches it.
    if html.is_empty()
        || (effects.is_empty() && !might_carry_style(html) && !might_contain_pre(html))
    {
        return Cow::Borrowed(html);
    }
    // The whitespace kindlegen writes in place of `<pre>` text changes these
    // bytes, so it is done first and everything else works on the result.
    let encoded = encode_pre_text(html);
    let html: &str = &encoded;
    // (offset, text) pairs, collected in document order.
    let mut inserts: Vec<(usize, String)> = Vec::new();
    // Source ranges to leave out: the `<pre>` tags themselves, which kindlegen
    // translates into markup rather than keeping.
    let mut drops: Vec<(usize, usize)> = Vec::new();
    // The element each open tag is waiting for, the markup to close it with,
    // and the spacer its bottom margin puts after it.
    let mut open_stack: Vec<(String, String, Option<String>)> = Vec::new();

    crate::links::for_each_tag(html, |tag, attrs| {
        if tag.closing {
            if let Some(at) = open_stack.iter().rposition(|(n, _, _)| *n == tag.name) {
                let (name, close, spacer) = open_stack.remove(at);
                if name == "pre" {
                    // The close the pre was opened with replaces this tag.
                    drops.push((tag.start, tag.end));
                }
                if !close.is_empty() {
                    inserts.push((tag.start, close));
                }
                // After the closing tag, so an inner element's spacer lands
                // inside its parent: measured, kindlegen nests them.
                if let Some(spacer) = spacer {
                    inserts.push((tag.end, spacer));
                }
            }
            return;
        }
        if tag.self_closing {
            // A `<pre/>` has no contents for a wrapper to hold, so kindlegen
            // writes its spacer and no more.
            if tag.name == "pre" {
                let top = effects
                    .get("pre", attrs)
                    .and_then(|e| e.height)
                    .unwrap_or_else(|| "0".to_string());
                drops.push((tag.start, tag.end));
                inserts.push((tag.start, format!("<div height=\"{top}\"></div>")));
            }
            return;
        }
        let effect = effects.get(&tag.name, attrs);
        // `<pre>` is the one element kindlegen does not keep. MOBI has no
        // `<pre>`, so it says the element in markup instead: a spacer for the
        // top margin (there is no element left to carry `height`), the indent,
        // then `<div align="left"><tt>`. Measured against kindlegen's own
        // output, including a `<pre>` no rule touches.
        if tag.name == "pre" {
            let effect = effect.unwrap_or_default();
            drops.push((tag.start, tag.end));
            let top = effect.height.as_deref().unwrap_or("0");
            let mut open = format!("<div height=\"{top}\"></div>");
            if effect.indent {
                open.push_str("<blockquote>");
            }
            open.push_str("<div align=\"left\"><tt>");
            inserts.push((tag.start, open));
            // Size, colour, bold, italic and underline go inside the `<tt>`,
            // where kindlegen puts them; the indent wraps the wrapper instead.
            let (wrapping_open, mut close) = effect.wrapping_markup();
            if !wrapping_open.is_empty() {
                inserts.push((tag.end, wrapping_open));
            }
            // kindlegen's own nesting is crossed here: it closes the
            // `<blockquote>` before the `<div>` and `<tt>` it opened inside it,
            // so `</blockquote></tt></div>` is what it writes. Kept as measured,
            // since a parser closes the same elements either way.
            if effect.indent {
                close.push_str("</blockquote>");
            }
            close.push_str("</tt></div>");
            open_stack.push(("pre".to_string(), close, effect.trailing_spacer()));
            return;
        }
        let effect = effect.unwrap_or_default();
        // `bgcolor` belongs on the element itself, so it goes just inside the
        // start tag's `>`. Self-closing elements were skipped above, which is
        // what makes the last byte of this span that `>`.
        if let Some(extra) = effect.start_attributes(&tag.name, attrs) {
            if tag.end > tag.start && html.as_bytes().get(tag.end - 1) == Some(&b'>') {
                inserts.push((tag.end - 1, extra));
            }
        }
        let (open, close) = effect.markup();
        if !open.is_empty() {
            inserts.push((tag.end, open));
        }
        // Every element that is not self-closing goes on the stack, with an
        // empty close when it has no markup of its own. Matching a close by
        // name alone finds the innermost *open* element of that name, so an
        // element that pushed nothing — a bare `<div>` inside a
        // `<div class="a">` — used to swallow the outer element's close: the
        // `<blockquote>` was closed at the inner `</div>` and whatever followed
        // it inside the outer element fell outside the indent.
        open_stack.push((tag.name.clone(), close, effect.trailing_spacer()));
    });

    // Source markup that never closes an element would otherwise leave the
    // tags this opened hanging open into the rest of the book. Close them at
    // the end, innermost first, with any spacer after its own close.
    while let Some((_, close, spacer)) = open_stack.pop() {
        if !close.is_empty() {
            inserts.push((html.len(), close));
        }
        if let Some(spacer) = spacer {
            inserts.push((html.len(), spacer));
        }
    }

    if inserts.is_empty() && drops.is_empty() {
        return encoded;
    }
    // A stable sort keeps two insertions at the same offset in the order they
    // were found, which is what makes the nesting come out right.
    inserts.sort_by_key(|(at, _)| *at);
    drops.sort_by_key(|(from, _)| *from);

    let mut out = String::with_capacity(html.len() + inserts.len() * 12);
    let mut cursor = 0usize;
    let mut next_drop = 0usize;
    for (at, text) in inserts {
        if at < cursor || at > html.len() {
            continue;
        }
        copy_upto(&mut out, html, cursor, at, &drops, &mut next_drop);
        out.push_str(&text);
        cursor = at;
    }
    copy_upto(&mut out, html, cursor, html.len(), &drops, &mut next_drop);
    Cow::Owned(out)
}

/// Copy `html[from..to]` to `out`, leaving out the ranges in `drops` that fall
/// inside it.
fn copy_upto(
    out: &mut String,
    html: &str,
    from: usize,
    to: usize,
    drops: &[(usize, usize)],
    next_drop: &mut usize,
) {
    let mut at = from;
    while *next_drop < drops.len() && drops[*next_drop].1 <= at {
        *next_drop += 1;
    }
    while *next_drop < drops.len() && drops[*next_drop].0 < to {
        let (start, end) = drops[*next_drop];
        if start > at {
            out.push_str(&html[at..start]);
        }
        at = at.max(end);
        *next_drop += 1;
        while *next_drop < drops.len() && drops[*next_drop].1 <= at {
            *next_drop += 1;
        }
    }
    if at < to {
        out.push_str(&html[at..to]);
    }
}

/// Whether the markup has a `<pre>` for [`apply`] to transliterate.
///
/// Cheap enough to run on every entry: the full scan only happens when the four
/// bytes are actually there.
fn might_contain_pre(html: &str) -> bool {
    html.as_bytes()
        .windows(4)
        .any(|w| w.eq_ignore_ascii_case(b"<pre"))
}

/// Encode the whitespace of `<pre>` contents the way kindlegen does.
///
/// MOBI has no `<pre>` and the popup has no stylesheet, so the whitespace has to
/// be spelled in markup: a newline becomes `<br/>`, a tab becomes
/// `&nbsp; &nbsp; ` and a carriage return is dropped. A run of spaces alternates
/// a plain space with `&nbsp;` so that a collapsing reader cannot eat it.
///
/// The phase of that alternation is measured, not derived. The first space of a
/// run is `&nbsp;` when the run follows a tag or the start of the element, and a
/// plain space when it follows text; from there it alternates. So two spaces at
/// an element's start are `&nbsp;&nbsp;`, two after a word are ` &nbsp;`, and a
/// single space at an element's start is a lone `&nbsp;` — a run of one, where a
/// toggle would write a plain space that collapses away. A tab or a newline
/// puts the next run back at the start-of-element phase; a form feed or any
/// other character does not, and is written as it is.
fn encode_pre_text(html: &str) -> Cow<'_, str> {
    if !might_contain_pre(html) {
        return Cow::Borrowed(html);
    }
    let mut out = String::with_capacity(html.len());
    let mut cursor = 0usize;
    let mut depth = 0usize;
    // Whether the last thing written inside the `<pre>` was a tag or the
    // element's own start, which is what decides the next run's phase.
    let mut after_tag = true;
    crate::links::for_each_tag(html, |tag, _attrs| {
        let gap = &html[cursor..tag.start];
        if depth == 0 {
            out.push_str(gap);
        } else {
            encode_pre_run(gap, &mut after_tag, &mut out);
        }
        out.push_str(&html[tag.start..tag.end]);
        if tag.name == "pre" {
            if tag.closing {
                depth = depth.saturating_sub(1);
            } else if !tag.self_closing {
                depth += 1;
            }
        }
        after_tag = true;
        cursor = tag.end;
    });
    let tail = &html[cursor..];
    if depth == 0 {
        out.push_str(tail);
    } else {
        encode_pre_run(tail, &mut after_tag, &mut out);
    }
    Cow::Owned(out)
}

/// Encode one run of `<pre>` text, which is everything between two tags.
fn encode_pre_run(text: &str, after_tag: &mut bool, out: &mut String) {
    // How many spaces the run has written so far, which is the phase of the
    // alternation.
    let mut run = 0usize;
    for c in text.chars() {
        match c {
            // A carriage return is dropped; `\r\n` leaves the newline's
            // `<br/>` behind, which is why kindlegen's output has one for a
            // CRLF line ending and none for a lone CR. It still ends whatever
            // run it sits in, and puts the next one at the after-text phase:
            // measured, `"><space><CR><space>"` writes `&nbsp; `, not
            // `&nbsp;&nbsp;`.
            '\r' => {
                run = 0;
                *after_tag = false;
            }
            '\n' => {
                out.push_str("<br/>");
                run = 0;
                *after_tag = true;
            }
            '\t' => {
                out.push_str("&nbsp; &nbsp; ");
                run = 0;
                *after_tag = false;
            }
            ' ' => {
                let nbsp = run % 2 == 1 || (run == 0 && *after_tag);
                out.push_str(if nbsp { "&nbsp;" } else { " " });
                run += 1;
                *after_tag = false;
            }
            c => {
                out.push(c);
                run = 0;
                *after_tag = false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn effects_of(css: &str) -> Effects {
        compile(css).effects
    }

    /// The effect `css` gives an element of this name with no class attribute.
    fn effect(css: &str, element: &str) -> Effect {
        effects_of(css)
            .get(element, "")
            .unwrap_or_else(|| panic!("{css:?} should match <{element}>"))
    }

    /// The effect `css` gives an element with this `class` attribute.
    fn effect_with(css: &str, element: &str, attrs: &str) -> Effect {
        effects_of(css)
            .get(element, attrs)
            .unwrap_or_else(|| panic!("{css:?} should match <{element} {attrs}>"))
    }

    /// Whether `css` asks for a `<blockquote>` indent on this element. A rule
    /// that matched but asked for nothing compiles to no effect at all, so this
    /// cannot use `effect`.
    fn indents(css: &str, element: &str) -> bool {
        effects_of(css)
            .get(element, "")
            .map(|e| e.indent)
            .unwrap_or(false)
    }

    /// The `class` tokens an attribute list carries, as the compiler reads
    /// them.
    fn classes_of(attrs: &str) -> Vec<String> {
        attribute_values(attrs).0
    }

    /// The value of an attribute list's `style` attribute, as the compiler
    /// reads it.
    fn style_of(attrs: &str) -> Option<String> {
        attribute_values(attrs).1.map(str::to_string)
    }

    /// The markup an entry actually ships: the compiler's insertions, then the
    /// strip pass that runs straight after it in the build, which is what takes
    /// the `class` and `style` attributes off the tags. Those are the bytes the
    /// lookup popup renders, so they are the ones worth comparing against
    /// kindlegen's.
    fn shipped(html: &str, effects: &Effects) -> String {
        crate::mobi::strip_idx_markup(&apply(html, effects))
    }

    #[test]
    fn compiles_the_properties_the_popup_honors() {
        let css = "b { font-weight: bold; } i { font-style: italic; } \
                   u { text-decoration: underline; } s { font-size: 200%; } \
                   q { color: green; } r { background-color: #eee; } \
                   t { margin-left: 2em; }";
        assert!(effect(css, "b").bold);
        assert!(effect(css, "i").italic);
        assert!(effect(css, "u").underline);
        assert_eq!(effect(css, "s").size, Some(3));
        assert_eq!(effect(css, "q").color.as_deref(), Some("green"));
        assert_eq!(effect(css, "r").background.as_deref(), Some("#eee"));
        assert!(effect(css, "t").indent);
    }

    #[test]
    fn writes_the_markup_kindlegen_writes() {
        // One `<font>` carrying both faces, `size` before `color`, the
        // `<blockquote>` outermost, and `bgcolor` on the element rather than
        // around its contents.
        let css = "p { font-size: 200%; font-weight: bold; font-style: italic; \
                   text-decoration: underline; color: red; margin-left: 1em; \
                   background-color: #eee; }";
        assert_eq!(
            apply("<p>COMBO</p>", &effects_of(css)),
            r##"<p bgcolor="#eee"><blockquote><font size="+3" color="red"><b><i><u>COMBO</u></i></b></font></blockquote></p>"##
        );
    }

    /// A `<pre>` is the one element kindlegen does not keep: MOBI has no
    /// `<pre>` and the popup no stylesheet, so it is replaced by a spacer for
    /// the top margin, the indent, and `<div align="left"><tt>`. Its whitespace
    /// is spelled out at the same time. Every expectation below is kindlegen's
    /// own output for the same source.
    #[test]
    fn a_pre_becomes_a_spacer_and_a_tt_wrapper() {
        // No stylesheet and no `style` attribute: the element still goes.
        assert_eq!(
            apply("<pre>x</pre>", &effects_of("")),
            r#"<div height="0"></div><div align="left"><tt>x</tt></div>"#
        );
        // The tags go, the id goes with them, and the text stays.
        assert_eq!(
            apply(r#"<pre id="k" class="c">x</pre>"#, &effects_of("")),
            r#"<div height="0"></div><div align="left"><tt>x</tt></div>"#
        );
        // A `<pre/>` has no contents for a wrapper to hold, so only the spacer
        // is written.
        assert_eq!(
            apply("<pre/>x", &effects_of("")),
            r#"<div height="0"></div>x"#
        );
    }

    #[test]
    fn a_pre_carries_its_margins_as_spacers_around_the_wrapper() {
        // The top margin cannot go on the element, because there is no element
        // left to carry it. kindlegen's own nesting is crossed here:
        // `</blockquote>` closes before the `<div>` and `<tt>` it was opened
        // outside of, and that is what it writes.
        assert_eq!(
            apply(
                r#"<pre style="margin:20px 1em 30px 1em">x</pre>"#,
                &effects_of("")
            ),
            r#"<div height="20"></div><blockquote><div align="left"><tt>x</blockquote></tt></div><div height="30"></div>"#
        );
    }

    #[test]
    fn a_pre_wraps_its_contents_inside_the_tt() {
        // Size, colour, bold, italic and underline go inside the `<tt>`, where
        // kindlegen puts them.
        assert_eq!(
            apply(
                r#"<pre style="font-size:260%;font-weight:bold;color:red">x</pre>"#,
                &effects_of("")
            ),
            r#"<div height="0"></div><div align="left"><tt><font size="+3" color="red"><b>x</b></font></tt></div>"#
        );
    }

    #[test]
    fn a_pre_nested_in_another_element_wraps_where_it_stands() {
        assert_eq!(
            apply("<div><pre>x</pre></div>", &effects_of("")),
            r#"<div><div height="0"></div><div align="left"><tt>x</tt></div></div>"#
        );
    }

    #[test]
    fn an_unclosed_pre_is_closed_at_the_end() {
        // Source markup that forgets `</pre>` must not leave the wrapper open
        // into the rest of the book.
        assert_eq!(
            apply("<pre>x", &effects_of("")),
            r#"<div height="0"></div><div align="left"><tt>x</tt></div>"#
        );
    }

    #[test]
    fn pre_whitespace_is_spelled_out() {
        // A run of spaces alternates a plain space with `&nbsp;` so a
        // collapsing reader cannot eat it, and the phase comes from what the
        // run follows: two spaces at the element's start are `&nbsp;&nbsp;`,
        // two after a word are ` &nbsp;`, and a single space at the element's
        // start is one `&nbsp;`.
        for (body, expected) in [
            (" a", "&nbsp;a"),
            ("  a", "&nbsp;&nbsp;a"),
            ("   a", "&nbsp;&nbsp; a"),
            ("a b", "a b"),
            ("a  b", "a &nbsp;b"),
            ("a   b", "a &nbsp; b"),
            ("a  ", "a &nbsp;"),
            // A tab is invariant: it always writes nbsp, space, nbsp, space.
            ("a\tb", "a&nbsp; &nbsp; b"),
            ("\ta", "&nbsp; &nbsp; a"),
            // A newline is a `<br/>`, and puts the next run back at the
            // start-of-element phase.
            ("a\nb", "a<br/>b"),
            ("a\n\nb", "a<br/><br/>b"),
            ("\n  a", "<br/>&nbsp;&nbsp;a"),
            ("a \nb", "a <br/>b"),
            // A carriage return is dropped, but it still ends a run.
            ("a\rb", "ab"),
            (" \r ", "&nbsp; "),
            ("a\r\nb", "a<br/>b"),
        ] {
            let html = format!("<pre>{body}</pre>");
            assert_eq!(
                apply(&html, &effects_of("")),
                format!(r#"<div height="0"></div><div align="left"><tt>{expected}</tt></div>"#),
                "{body:?}"
            );
        }
    }

    #[test]
    fn pre_whitespace_is_encoded_around_nested_elements() {
        // The encoding is per text run, and a tag ends the run: two spaces
        // after a nested element start at the element-start phase, and the
        // tab inside one is written the same way.
        assert_eq!(
            apply("<pre>a <span>b</span>  c</pre>", &effects_of("")),
            concat!(
                r#"<div height="0"></div><div align="left"><tt>a <span>b</span>"#,
                "&nbsp;&nbsp;c</tt></div>"
            )
        );
        assert_eq!(
            apply("<pre><span>a\tb</span></pre>", &effects_of("")),
            concat!(
                r#"<div height="0"></div><div align="left"><tt><span>"#,
                "a&nbsp; &nbsp; b</span></tt></div>"
            )
        );
    }

    #[test]
    fn only_pre_is_transliterated() {
        // The monospace elements beside it are left exactly as written: this
        // is `<pre>`'s rewrite alone, measured element by element.
        for element in [
            "tt",
            "code",
            "kbd",
            "samp",
            "var",
            "listing",
            "xmp",
            "plaintext",
        ] {
            let html = format!("<{element}>x</{element}>");
            assert_eq!(apply(&html, &effects_of("")), html, "{element}");
        }
    }

    #[test]
    fn a_colour_is_written_through_verbatim() {
        // kindlegen does not validate against a colour table, and neither does
        // this: measured, it ships `rgb(255,0,0)` and `transparent` as written.
        for value in ["red", "#ff0000", "#FFF", "rgb(255,0,0)", "transparent"] {
            let css = format!("span {{ color: {value}; }}");
            assert_eq!(
                apply("<span>t</span>", &effects_of(&css)),
                format!(r#"<span><font color="{value}">t</font></span>"#),
                "{value}"
            );
        }
        let css = "span { background-color: transparent; }";
        assert_eq!(
            apply("<span>t</span>", &effects_of(css)),
            r#"<span bgcolor="transparent">t</span>"#
        );
    }

    #[test]
    fn a_colour_that_would_escape_the_attribute_is_not_compiled() {
        let c = compile(r#"span { color: red" onclick="x; }"#);
        assert!(c.effects.is_empty());
        assert!(c.residual_css.contains("onclick"));
    }

    #[test]
    fn a_left_margin_indents_when_it_is_not_zero() {
        for value in ["20px", "1em", "5%", "0.5em", "1", "1.5px"] {
            let css = format!("span {{ margin-left: {value}; }}");
            assert!(indents(&css, "span"), "{value} should indent");
        }
        // `0.4em` is not zero, but kindlegen rounds it away and so does this:
        // the same rounding decides the indent, the height and the spacer.
        for value in [
            "0",
            "0px",
            "0.0em",
            "0.4em",
            "0.4px",
            "0.4%",
            "auto",
            "inherit",
            "-5px",
            "calc(1em + 2px)",
        ] {
            let css = format!("span {{ margin-left: {value}; }}");
            assert!(!indents(&css, "span"), "{value} should not indent");
            // Nothing was compiled from it, so the sheet keeps it.
            assert!(
                compile(&css).residual_css.contains("margin-left"),
                "{value} should stay in the sheet"
            );
        }
    }

    #[test]
    fn reads_each_side_of_a_margin_shorthand() {
        // One value is all four sides, two are vertical/horizontal, three are
        // top/horizontal/bottom, four are top/right/bottom/left.
        let sides = |declaration: &str| {
            let css = format!("p {{ {declaration}; }}");
            let e = effects_of(&css).get("p", "");
            (
                e.as_ref().and_then(|e| e.height.clone()),
                e.as_ref().and_then(|e| e.spacer.clone()),
                e.as_ref().map(|e| e.indent).unwrap_or(false),
            )
        };
        let v = |s: &str| Some(s.to_string());
        assert_eq!(sides("margin: 20px"), (v("20"), v("20"), true));
        assert_eq!(sides("margin: 20px 10px"), (v("20"), v("20"), true));
        assert_eq!(sides("margin: 20px 10px 30px"), (v("20"), v("30"), true));
        assert_eq!(
            sides("margin: 10px 20px 30px 2px"),
            (v("10"), v("30"), true)
        );
        // Vertical sides only.
        assert_eq!(sides("margin: 0 20px"), (None, None, true));
        // Every side rounds away, so nothing is expressed.
        assert_eq!(sides("margin: 0.3em 0"), (None, None, false));
        assert_eq!(sides("margin: 0"), (None, None, false));
        assert_eq!(sides("margin: auto"), (None, None, false));
        assert_eq!(sides("margin: 0.4em 0.4em"), (None, None, false));
        assert_eq!(sides("margin: 0px 0px 0px 12px"), (None, None, true));
    }

    #[test]
    fn a_margin_the_popup_cannot_use_stays_in_the_sheet() {
        // A margin with no legacy form, or one that rounds away, has to keep
        // reaching the book view.
        let c = compile(
            "p { margin-left: 2em; }
q { margin-top: 1.4em; }
r { margin-bottom: 1.4em; }
s { margin-right: 2em; }
t { padding-left: 2em; }
u { margin: 0.3em 0; }
v { margin-left: 0.4em; }",
        );
        // Expressed as an indent, a height or a spacer, so they leave.
        assert!(!c.residual_css.contains("margin-left: 2em"));
        assert!(!c.residual_css.contains("margin-top: 1.4em"));
        assert!(!c.residual_css.contains("margin-bottom: 1.4em"));
        assert!(c.residual_css.contains("margin-right: 2em"));
        assert!(c.residual_css.contains("padding-left: 2em"));
        assert!(c.residual_css.contains("margin: 0.3em 0"));
        assert!(c.residual_css.contains("margin-left: 0.4em"));
    }

    #[test]
    fn height_values_match_kindlegens_own_rounding() {
        // Each measured against kindlegen 2.9 one declaration at a time. The
        // units it does not know produce nothing at all.
        for (value, expected) in [
            ("20px", Some("20")),
            ("20", Some("20")),
            ("2.5px", Some("3")),
            ("1.5px", Some("2")),
            ("0.5px", Some("1")),
            ("2.4px", Some("2")),
            ("12pt", Some("16")),
            ("3pt", Some("4")),
            ("1in", Some("96")),
            ("1cm", Some("38")),
            ("10mm", Some("38")),
            ("1pc", Some("16")),
            ("2rem", Some("2")),
            ("1.4em", Some("1em")),
            ("0.5em", Some("1em")),
            ("2ex", Some("1em")),
            ("5%", Some("5%")),
            ("0.5%", Some("1%")),
            // Rounds away, negative, or not a length at all.
            ("0.4em", None),
            ("0.3em", None),
            ("0", None),
            ("auto", None),
            ("inherit", None),
            ("-5px", None),
            ("2ch", None),
            ("2vw", None),
            ("2vh", None),
            ("calc(1em + 2px)", None),
        ] {
            assert_eq!(
                height_value(value).as_deref(),
                expected,
                "height for {value:?}"
            );
        }
    }

    #[test]
    fn a_top_margin_lands_on_the_element_before_bgcolor() {
        // Declaration order does not matter: kindlegen writes `height` first.
        for css in [
            "span { background-color: #eee; margin-top: 20px; }",
            "span { margin-top: 20px; background-color: #eee; }",
        ] {
            assert_eq!(
                apply("<span>t</span>", &effects_of(css)),
                r##"<span height="20" bgcolor="#eee">t</span>"##
            );
        }
    }

    #[test]
    fn a_bottom_margin_puts_a_spacer_after_the_element() {
        let e = effects_of("span, p { margin-bottom: 20px; }");
        assert_eq!(
            apply("<span>t</span>", &e),
            r##"<span>t</span><div height="20"></div>"##
        );
        // Block elements too, and the spacers nest with the elements: an inner
        // element's lands before its parent's own closing tag.
        assert_eq!(
            apply("<p>t</p>", &e),
            r##"<p>t</p><div height="20"></div>"##
        );
        assert_eq!(
            apply("<span>o<span>i</span></span>", &e),
            r##"<span>o<span>i</span><div height="20"></div></span><div height="20"></div>"##
        );
    }

    #[test]
    fn one_margin_declaration_can_feed_all_three_sides() {
        let e = effects_of("p { margin: 20px 1em 30px 1em; }");
        assert_eq!(
            apply("<p>t</p>", &e),
            r##"<p height="20"><blockquote>t</blockquote></p><div height="30"></div>"##
        );
    }

    #[test]
    fn bgcolor_goes_on_the_element_not_around_it() {
        let e = effects_of("div { background-color: #eee; }");
        assert_eq!(
            apply(r#"<div class="x">t</div>"#, &e),
            r##"<div class="x" bgcolor="#eee">t</div>"##
        );
        // An element with no attributes at all, and one nested inside another.
        assert_eq!(
            apply("<div>a<div>b</div></div>", &e),
            r##"<div bgcolor="#eee">a<div bgcolor="#eee">b</div></div>"##
        );
    }

    #[test]
    fn an_attribute_only_effect_wraps_nothing() {
        let e = effects_of("div { background-color: #eee; }");
        assert_eq!(
            apply("<div>t</div>", &e),
            r##"<div bgcolor="#eee">t</div>"##
        );
    }

    #[test]
    fn an_unclosed_element_still_gets_its_attribute() {
        let e = effects_of("div { background-color: #eee; }");
        assert_eq!(apply("<div>t", &e), r##"<div bgcolor="#eee">t"##);
    }

    #[test]
    fn an_unclosed_element_still_gets_its_spacer() {
        let e = effects_of("div { margin-bottom: 20px; }");
        assert_eq!(apply("<div>t", &e), r##"<div>t<div height="20"></div>"##);
    }

    #[test]
    fn maps_260_percent_to_plus_three_like_kindlegen() {
        // The one mapping with an oracle behind it: kindlegen turns
        // `font-size: 260%` into `<font size="+3">`.
        assert_eq!(size_delta("260%"), Some(3));
        assert_eq!(size_delta("150%"), Some(2));
        assert_eq!(size_delta("120%"), Some(1));
        assert_eq!(size_delta("50%"), Some(-3));
        assert_eq!(size_delta("0.7em"), Some(-2));
        assert_eq!(size_delta("2em"), Some(3));
        assert_eq!(size_delta("x-large"), Some(2));
        // Too close to the base to be worth a tag.
        assert_eq!(size_delta("105%"), None);
        // No way to know the base, so no way to convert.
        assert_eq!(size_delta("12pt"), None);
        assert_eq!(size_delta("16px"), None);
    }

    #[test]
    fn element_and_class_selectors_compile() {
        let element = |s: &str| parse_selector(s).unwrap().element;
        let classes = |s: &str| parse_selector(s).unwrap().classes;

        assert_eq!(element("b"), Some("b".to_string()));
        assert_eq!(element(" idx\\:orth "), Some("idx:orth".to_string()));
        assert_eq!(element("H2"), Some("h2".to_string()));
        assert_eq!(element(".Def"), None);
        assert_eq!(classes(".Def"), vec!["def".to_string()]);
        assert_eq!(classes(" span.def "), vec!["def".to_string()]);
        assert_eq!(element("span.def"), Some("span".to_string()));
        assert_eq!(classes(".a.b"), vec!["a".to_string(), "b".to_string()]);

        for s in [
            "#id", "p > b", "p b", "a:hover", "[data-x]", "*", "", ".", "..a", ".a.", "span.",
        ] {
            assert!(parse_selector(s).is_none(), "{s:?} should not compile");
        }
    }

    #[test]
    fn a_selector_list_compiles_the_parts_it_can_read() {
        let e = effects_of("b, strong { font-weight: bold; }");
        assert!(e.get("b", "").unwrap().bold && e.get("strong", "").unwrap().bold);
        // A part that cannot be read keeps the rule in the sheet while the
        // parts beside it compile.
        let c = compile("b, #id { font-weight: bold; }");
        assert!(c.effects.get("b", "").unwrap().bold);
        assert!(c.effects.get("span", r#" id="id""#).is_none());
        assert!(c.residual_css.contains("#id"));
        assert!(c.residual_css.contains("font-weight: bold"));
        // A rule with nothing readable in it is kept exactly as it arrived.
        let c = compile("#id, [data-x] { font-weight: bold; }");
        assert!(c.effects.is_empty());
        assert_eq!(c.residual_css, "#id, [data-x] { font-weight: bold; }");
    }

    #[test]
    fn a_close_finds_the_element_that_is_actually_open() {
        // The outer `.a` div's content continues after a bare inner `<div>` of
        // the same name. Matching a close by name alone found the outer element
        // when the inner `</div>` arrived — the inner one had pushed nothing —
        // and closed the `<blockquote>` there, which put `after` outside the
        // indent. kindlegen nests an inner wrapper of its own on that element;
        // this is the shape kindling writes until it does.
        let e = effects_of(".a { margin-left: 1em; }");
        assert_eq!(
            apply(r#"<div class="a">before<div>inner</div>after</div>"#, &e),
            r#"<div class="a"><blockquote>before<div>inner</div>after</blockquote></div>"#
        );
        // A bare element with no wrapper of its own still closes what it should.
        assert_eq!(
            apply(r#"<div class="a"><p>x</p>y</div>"#, &e),
            r#"<div class="a"><blockquote><p>x</p>y</blockquote></div>"#
        );
    }

    #[test]
    fn a_descendant_selector_compiles_against_its_leading_compound() {
        // Measured on kindlegen 2.9: `.a .b` indents a `.a` even when the entry
        // holds no `.b` at all, so the compound before the space is what the
        // rule acts on.
        let e = effects_of(".a .b { margin-left: 1em; }  ");
        assert!(e.get("div", r#" class="a""#).unwrap().indent);
        assert!(e.get("span", r#" class="b""#).is_none());
        // A leading element, an element-qualified class and a class in an
        // inline style all reach their leading compound.
        let e = effects_of("span .b { font-size: .4em; } div.a .b { color: red; }");
        assert_eq!(e.get("span", "").unwrap().size, Some(-3));
        assert_eq!(
            e.get("div", r#" class="a""#).unwrap().color.as_deref(),
            Some("red")
        );
        // The rule stays in the sheet, because the elements the descendant half
        // names can only be styled from there.
        let c = compile("div.a .b { margin-left: 1em; }".to_string().as_str());
        assert!(c.residual_css.contains(".b"));
    }

    #[test]
    fn only_a_descendant_combinator_reads_as_its_leading_compound() {
        // Measured: `.a + .b`, `.a > .b` and `a.ref > *` produce nothing at all
        // in kindlegen, while `a.ref *` produces what `a.ref` asks for.
        for css in [
            ".a + .b { margin-left: 1em; }",
            ".a > .b { margin-left: 1em; }",
            "a.ref > * { font-style: italic; }",
        ] {
            let e = effects_of(css);
            assert!(e.get("div", r#" class="a""#).is_none(), "{css:?}");
            assert!(e.get("a", r#" class="ref""#).is_none(), "{css:?}");
        }
        let e = effects_of("a.ref * { font-style: italic; }");
        assert!(e.get("a", r#" class="ref""#).unwrap().italic);
        // A leading compound that carries an id is one this cannot match.
        let e = effects_of("#x .b { margin-left: 1em; }");
        assert!(e.is_empty());
        // A compound of several classes does match here, against the class
        // attribute that spells them.
        let e = effects_of(".a.master .b { font-style: italic; }");
        assert!(e.get("div", r#" class="a master""#).unwrap().italic);
    }

    #[test]
    fn an_unset_margin_takes_away_the_one_an_earlier_rule_asked_for() {
        // The real sheet's pair: `.subheadword { margin-left: 10px }` and
        // `.subheadword .sense { margin-left: unset }`. kindlegen leaves those
        // 21,890 elements unindented, and the `unset` is why.
        let e = effects_of(
            ".subheadword { margin-left: 10px; } \
             .subheadword .sense { margin-left: unset; }",
        );
        assert!(!e.get("div", r#" class="subheadword""#).unwrap().indent);
        // The other order keeps it, which is source order deciding as usual.
        let e = effects_of(
            ".subheadword .sense { margin-left: unset; } \
             .subheadword { margin-left: 10px; }",
        );
        assert!(e.get("div", r#" class="subheadword""#).unwrap().indent);
        // An `unset` under a selector that never had a margin adds nothing, and
        // does not make the entry walk run either.
        let c = compile(".related { margin-left: unset; }");
        assert!(c.effects.is_empty());
        assert!(c.residual_css.contains("margin-left: unset"));
        // An inline `unset` beats a rule the same way.
        let e = effects_of(".a { margin-left: 1em; }".to_string().as_str());
        assert!(
            !apply(r#"<div class="a" style="margin-left: unset">x</div>"#, &e)
                .contains("blockquote")
        );
        // The three sides and the shorthand all clear.
        let e = effects_of(
            "div { margin: 20px 1em 30px; } \
             div { margin-top: unset; margin-bottom: unset; }",
        );
        let eff = e.get("div", "").unwrap();
        assert!(
            eff.height.is_none() && eff.spacer.is_none() && eff.indent,
            "{eff:?}"
        );
        // And the shorthand clears all three sides at once.
        let e = effects_of("div { margin: 20px 1em 30px; } div { margin: unset; }");
        let eff = e.get("div", "").unwrap();
        assert!(
            !(eff.height.is_some() || eff.spacer.is_some() || eff.indent),
            "{eff:?}"
        );
    }

    #[test]
    fn one_unusable_part_no_longer_takes_the_others_with_it() {
        // The real Wadoku rule, with `.season small` in it: kindlegen compiles
        // the five parts it can read, and kindling used to drop all six.
        let css = "rt, .accent, i.genus, .descr, .etym, .season small \
                   { font-size: .4em; color: #3d3d3d; }";
        let e = effects_of(css);
        for (element, attrs) in [
            ("rt", ""),
            ("span", r#" class="accent""#),
            ("i", r#" class="genus""#),
            ("span", r#" class="descr""#),
            ("span", r#" class="etym""#),
        ] {
            let eff = e
                .get(element, attrs)
                .unwrap_or_else(|| panic!("{element}{attrs} was not compiled"));
            assert_eq!(eff.size, Some(-3), "{element}{attrs}");
            assert_eq!(eff.color.as_deref(), Some("#3d3d3d"), "{element}{attrs}");
        }
        // Only the unreadable part stays in the sheet, and it keeps the whole
        // declaration block, because nothing else can reach its elements.
        let c = compile(css);
        assert!(c.residual_css.contains(".season small"));
        assert!(c.residual_css.contains("font-size: .4em"));
        assert!(
            !c.residual_css.contains(".etym"),
            "a compiled part was left in the sheet: {:?}",
            c.residual_css
        );
    }

    #[test]
    fn a_comment_before_a_rule_does_not_disqualify_it() {
        // The real Wadoku sheet opens with a banner comment before `.poses`,
        // which the real build showed does not compile kindlegen's `<i>`.
        let c = compile(
            "/**\nMedia queries:\namzn-mobi\namzn-kf8\n */\n.poses { font-style: italic; }",
        );
        assert!(c.effects.get("span", r#" class="poses""#).unwrap().italic);
        assert_eq!(c.residual_css.trim(), "");
    }

    #[test]
    fn a_comment_inside_a_selector_list_does_not_disqualify_a_part() {
        let e = effects_of(".def, /* the other one */ .other { font-weight: bold; }");
        assert!(e.get("span", r#" class="def""#).unwrap().bold);
        assert!(e.get("span", r#" class="other""#).unwrap().bold);
        // And a comment *inside* the block is not a declaration either.
        let e = effects_of(".def { /* margin first */ font-weight: bold; }");
        assert!(e.get("span", r#" class="def""#).unwrap().bold);
    }

    #[test]
    fn a_comment_stays_where_the_rule_it_stands_in_stays() {
        // A rule this cannot use is written back whole, comment and all, so a
        // sheet kept for the reader keeps what its author wrote.
        let css = "/* why: the heading */\n.heading { padding-left: 2em; }";
        let c = compile(css);
        assert_eq!(c.residual_css, css);
        // An unterminated comment swallows the rule behind it, as a browser
        // swallows it, and leaves no effect behind.
        let c = compile("/* never closed\n.def { font-weight: bold; }");
        assert!(c.effects.is_empty());
        assert!(c.residual_css.starts_with("/* never closed"));
    }

    #[test]
    fn only_a_comment_is_removed_from_the_selector_text() {
        assert_eq!(without_comments(".a"), ".a");
        assert_eq!(without_comments("/* x */ .a"), " .a");
        assert_eq!(without_comments(".a/* x */.b"), ".a.b");
        assert_eq!(without_comments(".a /* one */ /* two */"), ".a  ");
        assert_eq!(without_comments("/* a */"), "");
        assert_eq!(without_comments(".a /* open"), ".a ");
    }

    #[test]
    fn a_class_selector_reaches_the_element_that_carries_the_class() {
        // The point of the feature: kindlegen resolves `.def` against the
        // source markup, and so does this now.
        let e = effects_of(".def { font-weight: bold; }");
        assert_eq!(
            apply(r#"<span class="def and more">a fruit</span>"#, &e),
            r#"<span class="def and more"><b>a fruit</b></span>"#
        );
        // An element without the class is untouched.
        assert_eq!(
            apply(r#"<span class="other">x</span>"#, &e),
            r#"<span class="other">x</span>"#
        );
    }

    #[test]
    fn an_element_qualified_class_selector_needs_both_halves() {
        let e = effects_of("span.def { font-weight: bold; }");
        assert!(e.get("span", r#" class="def""#).unwrap().bold);
        // Right class, wrong element.
        assert!(e.get("p", r#" class="def""#).is_none());
        // Right element, wrong class.
        assert!(e.get("span", r#" class="x""#).is_none());
    }

    #[test]
    fn every_class_in_a_compound_must_be_present() {
        let e = effects_of(".a.b { font-style: italic; }");
        assert!(e.get("span", r#" class="a b""#).unwrap().italic);
        assert!(e.get("span", r#" class="a""#).is_none());
        assert!(e.get("span", r#" class="a c""#).is_none());
    }

    #[test]
    fn a_class_selector_beats_a_bare_element_selector() {
        let e = effects_of("span { font-size: 150%; } .def { font-size: 260%; }");
        // The class rule is the more specific one, so its size wins.
        assert_eq!(e.get("span", r#" class="def""#).unwrap().size, Some(3));
        // While an element that does not carry it still gets the element rule.
        assert_eq!(e.get("span", "").unwrap().size, Some(2));
    }

    #[test]
    fn an_element_rule_and_a_class_rule_both_apply() {
        // Different properties, so neither has anything to override.
        let e = effects_of("p { font-weight: bold; } .x { font-style: italic; }");
        let eff = e.get("p", r#" class="x""#).unwrap();
        assert!(eff.bold && eff.italic);
    }

    #[test]
    fn class_names_reads_the_attribute_wherever_it_sits() {
        assert_eq!(
            classes_of(r#" class="def and more""#),
            ["def", "and", "more"]
        );
        assert_eq!(classes_of(r#" class='def'"#), ["def"]);
        assert_eq!(classes_of(r#" CLASS="Def""#), ["def"]);
        assert_eq!(classes_of(" class=def"), ["def"]);
        assert_eq!(
            classes_of(r#" id="x" class="def" style="color:red""#),
            ["def"]
        );
        assert_eq!(
            classes_of(r#" id="a" title="class=no""#),
            Vec::<String>::new()
        );
        assert_eq!(classes_of(r#" data-class="no""#), Vec::<String>::new());
        assert_eq!(classes_of(""), Vec::<String>::new());
    }

    #[test]
    fn class_names_skips_a_value_that_contains_a_quote() {
        assert_eq!(classes_of(r#" alt="a \"b" class="def""#), ["def"]);
    }

    #[test]
    fn the_style_attribute_is_read_wherever_it_sits() {
        // The value, not the whole attribute: `style="…"` quotes included in
        // the slice would reach `declarations` as part of every declaration.
        assert_eq!(
            style_of(r#" style="color:red""#).as_deref(),
            Some("color:red")
        );
        assert_eq!(
            style_of(r#" style='color:red'"#).as_deref(),
            Some("color:red")
        );
        assert_eq!(
            style_of(r#" STYLE="color:red""#).as_deref(),
            Some("color:red")
        );
        assert_eq!(
            style_of(r#" style = "color:red""#).as_deref(),
            Some("color:red")
        );
        assert_eq!(
            style_of(r#" class="def" style="color:red" id="k""#).as_deref(),
            Some("color:red")
        );
        // Nothing else named `style`, and no attribute at all.
        assert_eq!(style_of(r#" title="style=red""#), None);
        assert_eq!(style_of(r#" data-style="color:red""#), None);
        assert_eq!(style_of(""), None);
        // An empty value is still a value; only a missing attribute is `None`.
        assert_eq!(style_of(r#" style="""#).as_deref(), Some(""));
    }

    #[test]
    fn wraps_element_contents_leaving_the_tag_alone() {
        let e = effects_of("u { font-size: 260%; font-weight: bold; }");
        // The `<u>` tag itself must not move: an id on it is a link target.
        assert_eq!(
            apply(r#"<u id="k">UNDER</u>"#, &e),
            r#"<u id="k"><font size="+3"><b>UNDER</b></font></u>"#
        );
    }

    #[test]
    fn closes_in_the_reverse_order_it_opened() {
        let e =
            effects_of("p { font-weight: bold; font-style: italic; text-decoration: underline; }");
        assert_eq!(apply("<p>x</p>", &e), "<p><b><i><u>x</u></i></b></p>");
    }

    #[test]
    fn handles_nesting_of_the_same_element() {
        let e = effects_of("b { font-style: italic; }");
        assert_eq!(
            apply("<b>a<b>c</b>d</b>", &e),
            "<b><i>a<b><i>c</i></b>d</i></b>"
        );
    }

    #[test]
    fn leaves_unmatched_and_self_closing_elements_alone() {
        let e = effects_of("b { font-weight: bold; }");
        assert_eq!(apply("<p>x</p><br/><b/>", &e), "<p>x</p><br/><b/>");
    }

    #[test]
    fn closes_what_the_source_left_open() {
        let e = effects_of("b { font-weight: bold; }");
        assert_eq!(apply("<b>x", &e), "<b><b>x</b>");
    }

    #[test]
    fn does_nothing_without_rules() {
        assert_eq!(apply("<b>x</b>", &Effects::default()), "<b>x</b>");
    }

    #[test]
    fn compiled_declarations_leave_the_stylesheet() {
        // Otherwise a reader that does apply CSS would apply the rule and the
        // inline tag both, and double the effect.
        let c = compile("b { font-weight: bold; margin-right: 1em; }");
        assert!(c.effects.get("b", "").unwrap().bold);
        assert!(c.residual_css.contains("margin-right: 1em"));
        assert!(!c.residual_css.contains("font-weight"));
    }

    #[test]
    fn a_fully_compiled_rule_leaves_nothing_behind() {
        let c = compile("b { font-weight: bold; }");
        assert_eq!(c.residual_css.trim(), "");
    }

    #[test]
    fn a_rule_it_cannot_use_reaches_the_sheet_untouched() {
        // Not rebuilt from its parts: the reader gets the source bytes, which
        // keeps the shipped stylesheet a diff of the author's rather than a
        // reserialization of it.
        for css in [
            "p { margin: 0.3em 0; }",
            ".headword { padding-left: 2em; }",
            "@media screen { b { font-weight: bold; } }",
        ] {
            let c = compile(css);
            assert!(
                c.residual_css.contains(css),
                "{css:?} was not preserved verbatim, got {:?}",
                c.residual_css
            );
        }
    }

    #[test]
    fn an_at_rule_with_nested_rules_keeps_every_brace() {
        // Splitting on `}` used to eat the at-rule's own closing brace, which
        // left the shipped sheet unbalanced and swallowed everything after it.
        for css in [
            "@media screen { p { color: red } }",
            "@font-face { font-family: X; src: url(x.ttf); }",
            "@supports (display: flex) { .a { margin: 0 } b { color: red } }",
        ] {
            let c = compile(css);
            assert_eq!(c.residual_css, css, "{css:?} was not carried through whole");
            assert_eq!(
                c.residual_css.matches('{').count(),
                c.residual_css.matches('}').count(),
                "{css:?} came back with unbalanced braces"
            );
        }
    }

    #[test]
    fn a_rule_that_compiles_some_declarations_keeps_the_rest() {
        let c = compile(".def { font-weight: bold; margin-right: 1em; }");
        assert!(c.effects.get("span", r#" class="def""#).unwrap().bold);
        assert!(c.residual_css.contains(".def"));
        assert!(c.residual_css.contains("margin-right: 1em"));
        assert!(!c.residual_css.contains("font-weight"));
    }

    #[test]
    fn a_later_rule_wins_for_the_same_selector() {
        let e = effects_of("b { font-size: 120%; } b { font-size: 200%; }");
        assert_eq!(e.get("b", "").unwrap().size, Some(3));
        let e = effects_of(".x { font-size: 120%; } .x { font-size: 200%; }");
        assert_eq!(e.get("span", r#" class="x""#).unwrap().size, Some(3));
    }

    #[test]
    fn an_escaped_colon_selector_reaches_its_element() {
        // The rule kindling already moves to the end of the block for #39.
        let e = effects_of("idx\\:orth { font-weight: bold; }");
        assert_eq!(
            apply("<idx:orth>word</idx:orth>", &e),
            "<idx:orth><b>word</b></idx:orth>"
        );
    }

    #[test]
    fn survives_markup_that_would_break_a_regex() {
        let e = effects_of("b { font-weight: bold; }");
        let html = r#"<b title="a > b">x</b>"#;
        assert_eq!(apply(html, &e), r#"<b title="a > b"><b>x</b></b>"#);
    }

    #[test]
    fn an_element_without_a_class_gets_only_the_element_rule() {
        let e = effects_of(".def { font-weight: bold; } b { font-style: italic; }");
        let eff = e.get("b", r#" id="k""#).unwrap();
        assert!(eff.italic && !eff.bold);
        assert!(e.get("i", "").is_none());
    }

    #[test]
    fn merges_rules_across_style_blocks() {
        let c = compile_style_blocks(
            "<style>.def { font-weight: bold; }</style><style>.def { font-style: italic; }</style>",
        );
        let eff = c.effects.get("span", r#" class="def""#).unwrap();
        assert!(eff.bold && eff.italic);
    }

    #[test]
    fn compiles_an_elements_own_inline_style() {
        // Measured against kindlegen 2.9: one `style` attribute holding every
        // property comes back as the same tags, in the same nesting, that the
        // same declarations get from a rule.
        let html = concat!(
            r#"<span style="font-size:260%;font-weight:bold;font-style:italic;"#,
            r#"color:red;margin-left:1em;background-color:#eee;"#,
            r#"margin-top:20px;margin-bottom:20px">x</span>"#
        );
        assert_eq!(
            shipped(html, &Effects::default()),
            concat!(
                r##"<span height="20" bgcolor="#eee"><blockquote>"##,
                r##"<font size="+3" color="red"><b><i>x</i></b></font>"##,
                r##"</blockquote></span><div height="20"></div>"##
            )
        );
    }

    #[test]
    fn an_inline_style_compiles_one_property_at_a_time() {
        // Each of these is the markup kindlegen 2.9 writes for the same inline
        // declaration, read off one probe at a time.
        for (style, expected) in [
            ("font-style:italic", "<span><i>x</i></span>"),
            ("font-weight:bold", "<span><b>x</b></span>"),
            ("text-decoration:underline", "<span><u>x</u></span>"),
            ("color:red", r#"<span><font color="red">x</font></span>"#),
            ("font-size:260%", r#"<span><font size="+3">x</font></span>"#),
            ("margin-left:1em", "<span><blockquote>x</blockquote></span>"),
            (
                "background-color:#eee",
                r##"<span bgcolor="#eee">x</span>"##,
            ),
            ("margin-top:20px", r#"<span height="20">x</span>"#),
            (
                "margin-bottom:20px",
                r#"<span>x</span><div height="20"></div>"#,
            ),
            // Four sides: top becomes the height, left the blockquote, bottom
            // the spacer, and right has no legacy form at all.
            (
                "margin:20px 1em 30px 1em",
                r##"<span height="20"><blockquote>x</blockquote></span><div height="30"></div>"##,
            ),
        ] {
            let html = format!(r#"<span style="{style}">x</span>"#);
            assert_eq!(shipped(&html, &Effects::default()), expected, "{style}");
        }
    }

    #[test]
    fn an_inline_style_with_no_stylesheet_is_still_compiled() {
        // A dictionary whose styling is all inline ships no `<style>` block, so
        // there are no rules to compile and the attribute is all there is.
        let effects = Effects::default();
        assert!(effects.is_empty());
        assert_eq!(
            shipped(
                r#"<div class="sense" style="margin-left:1em">s</div>"#,
                &effects
            ),
            "<div><blockquote>s</blockquote></div>"
        );
    }

    #[test]
    fn an_inline_style_beats_every_rule_that_matched() {
        // Inline declarations are the most specific there are: measured,
        // kindlegen resolves an inline `font-size:260%` to `<font size="+3">`
        // over a sheet rule asking for 150%, and over the same class rule.
        for css in ["span { font-size: 150%; }", ".def { font-size: 150%; }"] {
            assert_eq!(
                shipped(
                    r#"<span class="def" style="font-size:260%">x</span>"#,
                    &effects_of(css)
                ),
                r#"<span><font size="+3">x</font></span>"#,
                "{css}"
            );
        }
    }

    #[test]
    fn a_class_rule_and_an_inline_style_both_apply() {
        // Different properties, so neither has anything to override: measured,
        // kindlegen writes `<b><i>x</i></b>` for this pair of declarations.
        assert_eq!(
            shipped(
                r#"<span class="def" style="font-style:italic">x</span>"#,
                &effects_of(".def { font-weight: bold; }")
            ),
            "<span><b><i>x</i></b></span>"
        );
    }

    #[test]
    fn an_inline_declaration_the_popup_cannot_use_compiles_to_nothing() {
        // Measured: kindlegen ships `<span>x</span>` for all three of these,
        // with no markup and no attribute, so nothing was compiled and nothing
        // was lost with the declarations.
        let e = Effects::default();
        for style in [
            "padding-left:2em;text-align:center",
            "font-size:12pt",
            "margin:0.3em 0",
        ] {
            let html = format!(r#"<span style="{style}">x</span>"#);
            assert_eq!(shipped(&html, &e), "<span>x</span>", "{style}");
        }
    }

    #[test]
    fn one_uncompilable_inline_declaration_does_not_sink_the_rest() {
        // Measured: `<span style="font-style:italic;padding-left:2em">` ships as
        // `<span><i>x</i></span>`.
        assert_eq!(
            shipped(
                r#"<span style="font-style:italic;padding-left:2em">x</span>"#,
                &Effects::default()
            ),
            "<span><i>x</i></span>"
        );
    }

    #[test]
    fn an_inline_style_is_read_through_quotes_and_whitespace() {
        let e = Effects::default();
        // Measured on kindlegen: a single-quoted value, and one written with
        // spaces around every colon and separator plus a capitalised property
        // and a trailing semicolon, both compile to the same markup a tidy
        // declaration list does.
        assert_eq!(
            shipped(r#"<span id="k" style='color:blue'>x</span>"#, &e),
            r#"<span id="k"><font color="blue">x</font></span>"#
        );
        assert_eq!(
            shipped(
                r#"<span id="k" style="  Color : red ; font-style: italic ; ">x</span>"#,
                &e
            ),
            r#"<span id="k"><font color="red"><i>x</i></font></span>"#
        );
        // An unrelated attribute whose value mentions `style` is not one.
        assert_eq!(
            apply(r#"<span title="style=red">x</span>"#, &e),
            r#"<span title="style=red">x</span>"#
        );
    }

    #[test]
    fn an_inline_style_on_the_entry_wrapper_wraps_the_whole_entry() {
        // Measured: a `style` on `<idx:entry>` itself reaches the entry's
        // contents, so kindlegen writes `<i><b>alpha</b><span>x</span></i>`.
        let out = shipped(
            concat!(
                r#"<idx:entry name="ja" style="font-style:italic">"#,
                r#"<idx:orth value="alpha"><b>alpha</b></idx:orth><span>x</span>"#,
                "</idx:entry>"
            ),
            &Effects::default(),
        );
        // The entry wrapper is gone and the `<i>` opens ahead of the headword
        // and closes after the last element, holding everything that was inside
        // the entry, which is where kindlegen puts it. The space between the
        // headword and the span is not this feature's doing: the strip pass
        // puts a separator back between a headword and the text that follows,
        // which kindling has always done and kindlegen does not.
        assert!(out.contains("<i><b>alpha</b>"), "{out}");
        assert!(out.contains("<span>x</span></i>"), "{out}");
        assert!(!out.contains("style="), "{out}");
    }

    #[test]
    fn reads_a_class_off_a_real_element() {
        // `for_each_tag` reports the attribute text between the name and the
        // `>`, which is what the scanner is fed here.
        let e = effects_of(".x { font-size: 150%; }");
        let eff = effect_with(".x { font-size: 150%; }", "span", r#" class="x" id="k""#);
        assert_eq!(eff.size, Some(2));
        assert_eq!(
            apply(r#"<span class="x">t</span>"#, &e),
            r#"<span class="x"><font size="+2">t</font></span>"#
        );
    }

    #[test]
    fn the_elements_that_carry_align_are_the_measured_ones() {
        // kindlegen 2.9, one element at a time: a short whitelist rather than
        // every block-level name, and nothing for a name it does not know.
        for element in [
            "p",
            "div",
            "td",
            "tr",
            "blockquote",
            "h1",
            "h2",
            "h3",
            "h4",
            "h5",
            "h6",
        ] {
            assert!(carries_align(element), "<{element}> should carry align");
        }
        for element in [
            "span", "b", "li", "ul", "ol", "table", "th", "pre", "section", "address", "caption",
            "foo", "h7", "td2", "idx:orth",
        ] {
            assert!(!carries_align(element), "<{element}> should get nothing");
        }
    }

    #[test]
    fn text_align_becomes_the_elements_own_align_attribute() {
        // The whole shape of this mapping, measured one element at a time.
        for element in ["p", "div", "td", "tr", "blockquote", "h1", "h2", "h6"] {
            let html = format!(r##"<{element} style="text-align:center">x</{element}>"##);
            assert_eq!(
                shipped(&html, &Effects::default()),
                format!(r##"<{element} align="center">x</{element}>"##),
                "<{element}>"
            );
        }
        // Elements kindlegen writes no attribute on, inline and block alike.
        for element in ["span", "b", "li", "ul", "table", "th", "section", "foo"] {
            let html = format!(r##"<{element} style="text-align:center">x</{element}>"##);
            assert_eq!(
                shipped(&html, &Effects::default()),
                format!("<{element}>x</{element}>"),
                "<{element}>"
            );
        }
    }

    #[test]
    fn a_text_align_value_is_written_through_verbatim() {
        // No validation and no lowercasing: kindlegen ships `CENTER`, `start`,
        // `inherit` and even a value that is not a keyword at all as written.
        for value in [
            "left",
            "right",
            "center",
            "justify",
            "start",
            "end",
            "inherit",
            "none",
            "CENTER",
            "Center",
            "foo",
            "-moz-center",
        ] {
            let html = format!(r#"<p style="text-align:{value}">x</p>"#);
            assert_eq!(
                shipped(&html, &Effects::default()),
                format!(r#"<p align="{value}">x</p>"#),
                "{value}"
            );
        }
        // A value that would close the attribute is refused, like a colour.
        let c = compile(r#"p { text-align: center" onclick="x; }"#);
        assert!(c.effects.is_empty());
        assert!(c.residual_css.contains("onclick"));
    }

    #[test]
    fn a_trailing_important_is_dropped_from_every_property() {
        // Measured: kindlegen compiles `color:red !important` to
        // `<font color="red">` and `text-align:center !important` to
        // `align="center"`, so the marker never reaches the attribute.
        assert_eq!(
            shipped(
                r#"<span style="color:red !important;font-size:260% !important">x</span>"#,
                &Effects::default()
            ),
            r#"<span><font size="+3" color="red">x</font></span>"#
        );
        assert_eq!(
            shipped(
                r#"<p style="text-align:center !important">x</p>"#,
                &Effects::default()
            ),
            r#"<p align="center">x</p>"#
        );
        for (value, expected) in [
            ("center !important", "center"),
            ("center!important", "center"),
            ("center ! IMPORTANT", "center"),
            ("center  !important  ", "center"),
            ("center", "center"),
            ("center !importantx", "center !importantx"),
            ("!important", ""),
        ] {
            assert_eq!(without_important(value), expected, "{value:?}");
        }
        // An uncompiled declaration goes back into the sheet as written.
        let c = compile("p { text-align: center !important; padding-left: 2em; }");
        assert!(c.residual_css.contains("padding-left: 2em"));
        assert!(!c.residual_css.contains("text-align"));
    }

    #[test]
    fn an_align_the_element_already_has_is_left_alone() {
        // Measured: kindlegen does not write a second attribute, quoted,
        // unquoted or empty alike, and still applies the rest of the style.
        for attrs in [
            r#" align="left""#,
            r#" align='left'"#,
            " align=left",
            r#" align="""#,
            r#" ALIGN="center""#,
        ] {
            let html = format!(r#"<p{attrs} style="text-align:center;font-weight:bold">x</p>"#);
            let expected = format!(r#"<p{attrs}><b>x</b></p>"#);
            assert_eq!(shipped(&html, &Effects::default()), expected, "{attrs}");
        }
        // The same through a class rule.
        assert_eq!(
            shipped(
                r#"<p align="left" class="x">x</p>"#,
                &effects_of(".x { text-align: center; }")
            ),
            r#"<p align="left">x</p>"#
        );
    }

    #[test]
    fn has_attribute_reads_the_name_not_the_value() {
        assert!(has_attribute(r#" align="left""#, "align"));
        assert!(has_attribute(" align=left", "align"));
        assert!(has_attribute(r#" align="""#, "align"));
        assert!(has_attribute(r#" class="x" ALIGN="center""#, "align"));
        assert!(!has_attribute(r#" data-align="x""#, "align"));
        assert!(!has_attribute(r#" title="align=left""#, "align"));
        assert!(!has_attribute(r#" class="align""#, "align"));
        assert!(!has_attribute("", "align"));
    }

    #[test]
    fn text_align_from_a_rule_reaches_its_element() {
        assert_eq!(
            shipped("<p>x</p>", &effects_of("p { text-align: center; }")),
            r#"<p align="center">x</p>"#
        );
        // A rule that names an element kindlegen cannot align compiles to
        // nothing, and stays in the sheet so the book view keeps it.
        let c = compile("span { text-align: center; }");
        assert!(c.effects.is_empty());
        assert!(c.residual_css.contains("text-align: center"));
        // A class rule is resolved against whatever carries the class: the
        // same rule aligns the `p` and leaves the `span` alone, which is what
        // kindlegen writes for one class and two elements.
        assert_eq!(
            shipped(
                r#"<p class="x">p</p><span class="x">s</span>"#,
                &effects_of(".x { text-align: center; }")
            ),
            r#"<p align="center">p</p><span>s</span>"#
        );
        assert_eq!(
            shipped(
                r#"<table><tr><td class="x">x</td></tr></table>"#,
                &effects_of(".x { text-align: center; }")
            ),
            r#"<table><tr><td align="center">x</td></tr></table>"#
        );
    }

    #[test]
    fn a_rule_for_an_element_that_cannot_align_keeps_the_declaration() {
        for css in [
            "span { text-align: center; }",
            "li { text-align: center; }",
            "table { text-align: center; }",
            "th { text-align: center; }",
            "span.def { text-align: center; }",
        ] {
            let c = compile(css);
            assert!(
                c.residual_css.contains(css),
                "{css:?} should stay in the sheet, got {:?}",
                c.residual_css
            );
        }
        // A rule that can reach an aligning element is compiled whole, even
        // when another selector in the list cannot.
        for css in [
            "p { text-align: center; }",
            ".def { text-align: center; }",
            "p, span { text-align: center; }",
            "td.def { text-align: center; }",
        ] {
            let c = compile(css);
            assert_eq!(c.residual_css.trim(), "", "{css:?} should compile away");
        }
        // The rest of a rule that could not compile still leaves.
        let c = compile("li { font-weight: bold; text-align: center; }");
        assert!(c.effects.get("li", "").unwrap().bold);
        assert!(c.residual_css.contains("text-align: center"));
        assert!(!c.residual_css.contains("font-weight"));
    }

    #[test]
    fn an_inline_text_align_beats_a_rule_and_everything_the_element_carries() {
        // Measured: a `left` rule with an inline `center` ships
        // `align="center"`.
        assert_eq!(
            shipped(
                r#"<p style="text-align:center">x</p>"#,
                &effects_of("p { text-align: left; }")
            ),
            r#"<p align="center">x</p>"#
        );
        // Two declarations of the same property: the later one wins, inline
        // and in a rule alike.
        assert_eq!(
            shipped(
                r#"<p style="text-align:left;text-align:right">x</p>"#,
                &Effects::default()
            ),
            r#"<p align="right">x</p>"#
        );
        assert_eq!(
            shipped(
                "<p>x</p>",
                &effects_of("p { text-align: left; text-align: right; }")
            ),
            r#"<p align="right">x</p>"#
        );
    }

    #[test]
    fn align_sits_between_height_and_bgcolor() {
        // Measured, and independent of declaration order:
        // `<p height="20" align="center" bgcolor="#eee">`.
        for style in [
            "background-color:#eee;margin-top:20px;text-align:center",
            "text-align:center;background-color:#eee;margin-top:20px",
        ] {
            let html = format!(r#"<p style="{style}">x</p>"#);
            assert_eq!(
                shipped(&html, &Effects::default()),
                r##"<p height="20" align="center" bgcolor="#eee">x</p>"##,
                "{style}"
            );
        }
        // The alignment is on the element, so the wrapping tags go inside it.
        assert_eq!(
            shipped(
                r#"<p style="text-align:center;font-weight:bold;margin-bottom:20px">x</p>"#,
                &Effects::default()
            ),
            concat!(
                r##"<p align="center"><b>x</b></p>"##,
                r##"<div height="20"></div>"##
            )
        );
    }

    #[test]
    fn a_wrapping_effect_is_untouched_by_the_align_whitelist() {
        // `text-align` on a span does nothing, but its other declarations must
        // still compile where they can.
        let e = Effects::default();
        assert_eq!(
            shipped(
                r#"<span style="text-align:center;font-style:italic">x</span>"#,
                &e
            ),
            "<span><i>x</i></span>"
        );
        assert_eq!(
            shipped(r#"<div style="text-align:center"><span>x</span></div>"#, &e),
            r#"<div align="center"><span>x</span></div>"#
        );
    }
}
