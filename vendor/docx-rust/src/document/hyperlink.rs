#![allow(unused_must_use)]

use hard_xml::{XmlRead, XmlWrite};
use std::borrow::Cow;

use crate::{__setter, __xml_test_suites, document::Run};

/// The root element of a hyperlink within the paragraph
#[derive(Debug, Default, XmlRead, XmlWrite, Clone)]
#[cfg_attr(test, derive(PartialEq))]
#[xml(tag = "w:hyperlink")]
pub struct Hyperlink<'a> {
    /// Specifies the ID of the relationship in the relationships part for an external link.
    #[xml(attr = "r:id")]
    pub id: Option<Cow<'a, str>>,
    /// Specifies the name of a bookmark within the document.
    #[xml(attr = "w:anchor")]
    pub anchor: Option<Cow<'a, str>>,
    #[xml(child = "w:r")]
    /// Link content
    pub content: Run<'a>,
}

impl<'a> Hyperlink<'a> {
    __setter!(id: Option<Cow<'a, str>>);
    __setter!(anchor: Option<Cow<'a, str>>);
    __setter!(content: Run<'a>);
}

__xml_test_suites!(
    Hyperlink,
    Hyperlink::default(),
    r#"<w:hyperlink><w:r/></w:hyperlink>"#,
    Hyperlink::default().id("id"),
    r#"<w:hyperlink r:id="id"><w:r/></w:hyperlink>"#,
    Hyperlink::default().anchor("anchor"),
    r#"<w:hyperlink w:anchor="anchor"><w:r/></w:hyperlink>"#,
);
