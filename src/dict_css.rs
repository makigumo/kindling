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
    }

    /// Fold `other` over this, `other` winning.
    ///
    /// This is how two rules that both match an element are combined, so
    /// `other` is the more specific one (or the later of two equals): it
    /// replaces a size or a colour it declares and turns on the attributes it
    /// asks for, leaving alone everything it says nothing about. A declaration
    /// that turns a property *off* is not compiled at all — `font-weight:
    /// normal` stays in the sheet — so there is nothing here that relaxes a
    /// flag.
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
        }
        if other.spacer.is_some() {
            self.spacer = other.spacer.clone();
        }
        self.bold |= other.bold;
        self.italic |= other.italic;
        self.underline |= other.underline;
        self.indent |= other.indent;
    }

    /// The markup to open and close around an element's contents. Closing
    /// order mirrors opening order so the result nests properly.
    ///
    /// The order is kindlegen's, measured: `margin-left` becomes the
    /// outermost `<blockquote>`, then one `<font>` carrying size and colour,
    /// then `<b>`, `<i>`, `<u>`. Its own output for a `<p>` holding all of them
    /// is `<p bgcolor="#eee"><blockquote><font size="+3" color="red"><b><i><u>…`.
    fn markup(&self) -> (String, String) {
        let mut open = String::new();
        let mut close = String::new();
        if self.indent {
            open.push_str("<blockquote>");
            close.insert_str(0, "</blockquote>");
        }
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
    /// Two declarations that are not wrapped around the contents, because
    /// kindlegen puts them on the element itself: a top margin becomes
    /// `height`, and `background-color` becomes `bgcolor`. Measured, in that
    /// order however the declarations were written — `<span
    /// style="background-color:#eee;margin-top:20px">` and the reverse both
    /// ship as `<span height="20" bgcolor="#eee">` — so these have to land
    /// before the `>` that closes the start tag. The `bgcolor` half is also the
    /// reason the IDA work found the writer reading a colour out of its style
    /// struct in the branch that emits `bgcolor`.
    fn start_attributes(&self) -> Option<String> {
        let mut attrs = String::new();
        if let Some(h) = &self.height {
            attrs.push_str(&format!(" height=\"{}\"", h));
        }
        if let Some(c) = &self.background {
            attrs.push_str(&format!(" bgcolor=\"{}\"", c));
        }
        (!attrs.is_empty()).then_some(attrs)
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

    /// Record one compiled rule under the selector that asked for it.
    fn insert(&mut self, selector: &Selector, effect: &Effect) {
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

    /// The effect an element should carry, if any rule matches it.
    ///
    /// `attrs` is the element's raw attribute text. It is only parsed when a
    /// class rule exists, so a stylesheet of element selectors alone costs
    /// nothing extra on the way through a dictionary's half a million entries.
    fn get(&self, element: &str, attrs: &str) -> Option<Effect> {
        let mut effect = self.elements.get(element).cloned();
        if self.classes.is_empty() {
            return effect;
        }
        let classes = class_names(attrs);
        if classes.is_empty() {
            return effect;
        }

        // Everything that matches, weakest first: a class selector beats a bare
        // element selector, and one requiring more classes beats one requiring
        // fewer. Source order decides between equal specificity, which a stable
        // sort preserves.
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
        effect
    }
}

/// The tokens of an element's `class` attribute, lowercased.
///
/// This is a scanner rather than a search for `class=` because that string
/// turns up inside other attributes' values; it walks the attribute list the
/// way a parser would, skipping each value whole.
///
/// Class names are compared case-insensitively, unlike CSS, where the match is
/// case-sensitive. Element names here are folded the same way, and a sheet
/// written as `.Def` against markup that says `class="Def"` agrees either way;
/// folding only stops a sheet and its markup that disagree in case from
/// silently doing nothing.
fn class_names(attrs: &str) -> Vec<String> {
    let bytes = attrs.as_bytes();
    let mut out: Vec<String> = Vec::new();
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
        }
    }
    out
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
fn declarations(block: &str) -> Vec<(String, String, String)> {
    block
        .split(';')
        .filter(|d| !d.trim().is_empty())
        .filter_map(|d| {
            let (name, value) = d.split_once(':')?;
            Some((
                name.trim().to_ascii_lowercase(),
                value.trim().to_string(),
                d.to_string(),
            ))
        })
        .collect()
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

/// A colour declaration's value, ready to sit inside an attribute.
///
/// kindlegen passes the value through untranslated — measured: `red`, `#ff0000`
/// and even `rgb(255,0,0)` and `transparent` all reach the shipped text as
/// written — so nothing is normalised or validated against a colour table here.
/// A value carrying a quote or an angle bracket is refused instead, because it
/// would close the attribute and let the rest of the declaration escape into
/// the markup.
fn color_value(value: &str) -> Option<String> {
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

        let parts: Vec<&str> = selectors.split(',').collect();
        let parsed: Option<Vec<Selector>> = parts.iter().map(|p| parse_selector(p)).collect();
        let Some(parsed) = parsed.filter(|p| !p.is_empty()) else {
            // One unusable part disqualifies the rule: putting back only the
            // parts that compiled would drop styling the sheet asked for.
            residual.push_str(whole);
            continue;
        };

        let mut effect = Effect::default();
        let mut kept: Vec<String> = Vec::new();
        for (prop, value, original) in declarations(block) {
            let mut consumed = true;
            match prop.as_str() {
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
                "color" => match color_value(&value) {
                    Some(c) => effect.color = Some(c),
                    None => consumed = false,
                },
                "background-color" => match color_value(&value) {
                    Some(c) => effect.background = Some(c),
                    None => consumed = false,
                },
                "margin-top" => match height_value(&value) {
                    Some(h) => effect.height = Some(h),
                    None => consumed = false,
                },
                "margin-bottom" => match height_value(&value) {
                    Some(h) => effect.spacer = Some(h),
                    None => consumed = false,
                },
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
                    if let Some(h) = top.and_then(height_value) {
                        effect.height = Some(h);
                        expressed = true;
                    }
                    if let Some(h) = bottom.and_then(height_value) {
                        effect.spacer = Some(h);
                        expressed = true;
                    }
                    if left.and_then(height_value).is_some() {
                        effect.indent = true;
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

        if !effect.is_empty() {
            for selector in &parsed {
                effects.insert(selector, &effect);
            }
        }
        // Put the rule back only if something in it was not compiled. A rule
        // that compiled nothing is written back exactly as it arrived rather
        // than rebuilt, so a sheet this cannot use still reaches the reader
        // unchanged down to its spacing.
        if !kept.is_empty() {
            if effect.is_empty() {
                residual.push_str(whole);
            } else {
                residual.push_str(selectors);
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
/// Insertions go inside the element rather than around it, so an element that
/// carries an id stays where a link expects to find it. Nothing else in the
/// markup moves, which matters because dictionary entry offsets are measured
/// on these bytes afterwards.
pub(crate) fn apply<'a>(html: &'a str, effects: &Effects) -> Cow<'a, str> {
    if effects.is_empty() || html.is_empty() {
        return Cow::Borrowed(html);
    }
    // (offset, text) pairs, collected in document order.
    let mut inserts: Vec<(usize, String)> = Vec::new();
    // The element each open tag is waiting for, the markup to close it with,
    // and the spacer its bottom margin puts after it.
    let mut open_stack: Vec<(String, String, Option<String>)> = Vec::new();

    crate::links::for_each_tag(html, |tag, attrs| {
        if tag.closing {
            if let Some(at) = open_stack.iter().rposition(|(n, _, _)| *n == tag.name) {
                let (_, close, spacer) = open_stack.remove(at);
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
            return;
        }
        let Some(effect) = effects.get(&tag.name, attrs) else {
            return;
        };
        // `bgcolor` belongs on the element itself, so it goes just inside the
        // start tag's `>`. Self-closing elements were skipped above, which is
        // what makes the last byte of this span that `>`.
        if let Some(extra) = effect.start_attributes() {
            if tag.end > tag.start && html.as_bytes().get(tag.end - 1) == Some(&b'>') {
                inserts.push((tag.end - 1, extra));
            }
        }
        let (open, close) = effect.markup();
        let spacer = effect.trailing_spacer();
        // An effect can be an attribute or a trailing spacer alone, with
        // nothing to wrap.
        if !open.is_empty() || spacer.is_some() {
            if !open.is_empty() {
                inserts.push((tag.end, open));
            }
            open_stack.push((tag.name.clone(), close, spacer));
        }
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

    if inserts.is_empty() {
        return Cow::Borrowed(html);
    }
    // A stable sort keeps two insertions at the same offset in the order they
    // were found, which is what makes the nesting come out right.
    inserts.sort_by_key(|(at, _)| *at);

    let mut out = String::with_capacity(html.len() + inserts.len() * 12);
    let mut cursor = 0usize;
    for (at, text) in inserts {
        if at < cursor || at > html.len() {
            continue;
        }
        out.push_str(&html[cursor..at]);
        out.push_str(&text);
        cursor = at;
    }
    out.push_str(&html[cursor..]);
    Cow::Owned(out)
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
    fn a_selector_list_compiles_only_when_every_part_does() {
        let e = effects_of("b, strong { font-weight: bold; }");
        assert!(e.get("b", "").unwrap().bold && e.get("strong", "").unwrap().bold);
        // One unusable part disqualifies the whole rule, so the sheet keeps it.
        let c = compile("b, #id { font-weight: bold; }");
        assert!(c.effects.is_empty());
        assert!(c.residual_css.contains("#id"));
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
            class_names(r#" class="def and more""#),
            ["def", "and", "more"]
        );
        assert_eq!(class_names(r#" class='def'"#), ["def"]);
        assert_eq!(class_names(r#" CLASS="Def""#), ["def"]);
        assert_eq!(class_names(" class=def"), ["def"]);
        assert_eq!(
            class_names(r#" id="x" class="def" style="color:red""#),
            ["def"]
        );
        assert_eq!(
            class_names(r#" id="a" title="class=no""#),
            Vec::<String>::new()
        );
        assert_eq!(class_names(r#" data-class="no""#), Vec::<String>::new());
        assert_eq!(class_names(""), Vec::<String>::new());
    }

    #[test]
    fn class_names_skips_a_value_that_contains_a_quote() {
        assert_eq!(class_names(r#" alt="a \"b" class="def""#), ["def"]);
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
}
