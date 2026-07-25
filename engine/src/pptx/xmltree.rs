//! Minimal read-only XML DOM used by the PPTX parsers.
//! Copyright (c) 2026 Anicca Code Studio. MIT licensed.
//!
//! Slides are small enough that building a tree is cheaper (in developer time
//! and bug surface) than threading quick-xml state through deeply nested
//! DrawingML. Element names are stored *local* (namespace prefix stripped);
//! the original prefix rarely matters for the subset we consume.

use std::collections::HashMap;

use quick_xml::events::Event;
use quick_xml::Reader;

use super::local;

#[derive(Debug, Clone)]
pub struct Node {
    /// Local element name (no namespace prefix).
    pub name: String,
    pub attrs: HashMap<String, String>,
    pub children: Vec<Node>,
    pub text: String,
}

impl Node {
    fn new(name: String) -> Node {
        Node { name, attrs: HashMap::new(), children: Vec::new(), text: String::new() }
    }

    /// First direct child with the given local name.
    pub fn child(&self, name: &str) -> Option<&Node> {
        self.children.iter().find(|c| c.name == name)
    }

    /// All direct children with the given local name.
    pub fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Node> {
        self.children.iter().filter(move |c| c.name == name)
    }

    /// Attribute value by local name.
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.get(name).map(|s| s.as_str())
    }

    pub fn attr_i64(&self, name: &str) -> Option<i64> {
        self.attr(name).and_then(|v| v.trim().parse::<i64>().ok())
    }

    pub fn attr_f64(&self, name: &str) -> Option<f64> {
        self.attr(name).and_then(|v| v.trim().parse::<f64>().ok())
    }

    /// Depth-first descendant search by local name (first match).
    pub fn find(&self, name: &str) -> Option<&Node> {
        for c in &self.children {
            if c.name == name {
                return Some(c);
            }
            if let Some(n) = c.find(name) {
                return Some(n);
            }
        }
        None
    }

    /// Concatenated text of this node and descendants.
    pub fn text_content(&self) -> String {
        let mut out = self.text.clone();
        for c in &self.children {
            out.push_str(&c.text_content());
        }
        out
    }
}

/// Parse an XML document into a tree. Returns the root element.
pub fn parse(xml: &str) -> Option<Node> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut stack: Vec<Node> = Vec::new();
    let mut root: Option<Node> = None;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = String::from_utf8_lossy(local(e.name().as_ref())).into_owned();
                let mut node = Node::new(name);
                for a in e.attributes().flatten() {
                    let k = String::from_utf8_lossy(a.key.local_name().as_ref()).into_owned();
                    let v = String::from_utf8_lossy(&a.value).into_owned();
                    node.attrs.insert(k, v);
                }
                stack.push(node);
            }
            Ok(Event::Empty(e)) => {
                let name = String::from_utf8_lossy(local(e.name().as_ref())).into_owned();
                let mut node = Node::new(name);
                for a in e.attributes().flatten() {
                    let k = String::from_utf8_lossy(a.key.local_name().as_ref()).into_owned();
                    let v = String::from_utf8_lossy(&a.value).into_owned();
                    node.attrs.insert(k, v);
                }
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(node);
                } else {
                    root = Some(node);
                }
            }
            Ok(Event::Text(t)) => {
                if let Ok(txt) = t.unescape() {
                    if let Some(top) = stack.last_mut() {
                        top.text.push_str(&txt);
                    }
                }
            }
            Ok(Event::End(_)) => {
                if let Some(node) = stack.pop() {
                    if let Some(parent) = stack.last_mut() {
                        parent.children.push(node);
                    } else {
                        root = Some(node);
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    root
}
