/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

// Namespace handling for the XML serialization path. html5ever's serializer
// remains responsible for HTML. xml5ever's writer does not generate attribute
// prefixes or track explicit namespace declarations, so handle element tags
// here and reuse its non-element writers.
use std::collections::BTreeMap;
use std::io::{self, Write};

use html5ever::serialize::{AttrRef, Serialize, Serializer, TraversalScope};
use html5ever::QualName;
use xml5ever::serialize::XmlSerializer;

use crate::dom::node::Node;
use crate::dom::servoparser::html::HtmlSerialize;

const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";
const XMLNS_NS: &str = "http://www.w3.org/2000/xmlns/";

type Bindings = BTreeMap<String, String>;

fn is_namespace_declaration(name: &QualName) -> bool {
    name.ns.as_ref() == XMLNS_NS ||
        (name.ns.is_empty() && name.local.as_ref() == "xmlns")
}

struct ElementScope {
    bindings: Bindings,
    qualified_name: String,
}

struct NamespaceSerializer<W> {
    writer: W,
    scopes: Vec<ElementScope>,
    next_prefix: usize,
}

pub(crate) fn serialize<W: Write>(
    writer: W,
    node: &Node,
    traversal_scope: TraversalScope,
) -> io::Result<()> {
    let mut serializer = NamespaceSerializer {
        writer,
        scopes: vec![],
        next_prefix: 1,
    };
    HtmlSerialize::new_xml(node).serialize(&mut serializer, traversal_scope)
}

impl<W: Write> NamespaceSerializer<W> {
    fn bind(
        bindings: &mut Bindings,
        declarations: &mut Bindings,
        prefix: &str,
        namespace: &str,
    ) {
        if bindings.get(prefix).map(String::as_str) != Some(namespace) {
            bindings.insert(prefix.to_owned(), namespace.to_owned());
            declarations.insert(prefix.to_owned(), namespace.to_owned());
        }
    }

    fn prefix_for(
        &mut self,
        bindings: &mut Bindings,
        declarations: &mut Bindings,
        preferred: Option<&str>,
        namespace: &str,
    ) -> String {
        if namespace == XML_NS {
            return "xml".to_owned();
        }
        if let Some(prefix) = preferred.filter(|prefix| !prefix.is_empty()) {
            match bindings.get(prefix) {
                Some(value) if value == namespace => return prefix.to_owned(),
                None => {
                    Self::bind(bindings, declarations, prefix, namespace);
                    return prefix.to_owned();
                },
                _ => {},
            }
        }
        if let Some((prefix, _)) = bindings
            .iter()
            .find(|(prefix, value)| !prefix.is_empty() && value.as_str() == namespace)
        {
            return prefix.clone();
        }
        loop {
            let prefix = format!("ns{}", self.next_prefix);
            self.next_prefix += 1;
            if !bindings.contains_key(&prefix) {
                Self::bind(bindings, declarations, &prefix, namespace);
                return prefix;
            }
        }
    }

    // XML attribute normalization would otherwise replace literal whitespace.
    fn write_attribute(&mut self, name: &str, value: &str) -> io::Result<()> {
        write!(self.writer, " {name}=\"")?;
        for character in value.chars() {
            match character {
                '&' => self.writer.write_all(b"&amp;")?,
                '<' => self.writer.write_all(b"&lt;")?,
                '>' => self.writer.write_all(b"&gt;")?,
                '"' => self.writer.write_all(b"&quot;")?,
                '\t' => self.writer.write_all(b"&#9;")?,
                '\n' => self.writer.write_all(b"&#10;")?,
                '\r' => self.writer.write_all(b"&#13;")?,
                character => write!(self.writer, "{character}")?,
            }
        }
        self.writer.write_all(b"\"")
    }
}

impl<W: Write> Serializer for NamespaceSerializer<W> {
    fn start_elem<'a, AttrIter>(&mut self, name: QualName, attrs: AttrIter) -> io::Result<()>
    where
        AttrIter: Iterator<Item = AttrRef<'a>>,
    {
        let attrs: Vec<_> = attrs.collect();
        let mut bindings = self.scopes.last().map_or_else(
            || Bindings::from([
                (String::new(), String::new()),
                ("xml".to_owned(), XML_NS.to_owned()),
            ]),
            |scope| scope.bindings.clone(),
        );
        let mut declarations = Bindings::new();

        // Read all declarations first, including those after a prefixed
        // attribute. They participate in prefix selection for the whole tag.
        for (attribute, value) in &attrs {
            if !is_namespace_declaration(attribute) {
                continue;
            }
            let prefix = if attribute.prefix.is_none() && attribute.local.as_ref() == "xmlns" {
                ""
            } else {
                attribute.local.as_ref()
            };
            // The XML namespace has a predefined binding and may not be
            // assigned a different prefix or a default namespace.
            if *value == XML_NS || prefix == "xml" {
                continue;
            }
            bindings.insert(prefix.to_owned(), (*value).to_owned());
            declarations.insert(prefix.to_owned(), (*value).to_owned());
        }

        let namespace = name.ns.as_ref();
        let qualified_name = if namespace.is_empty() {
            Self::bind(&mut bindings, &mut declarations, "", "");
            name.local.to_string()
        } else if namespace == XML_NS || name.prefix.is_some() {
            let prefix = self.prefix_for(
                &mut bindings,
                &mut declarations,
                name.prefix.as_ref().map(|prefix| prefix.as_ref()),
                namespace,
            );
            format!("{prefix}:{}", name.local)
        } else {
            Self::bind(&mut bindings, &mut declarations, "", namespace);
            name.local.to_string()
        };

        let mut attributes = Vec::with_capacity(attrs.len());
        for (attribute, value) in attrs {
            let namespace = attribute.ns.as_ref();
            if is_namespace_declaration(attribute) {
                continue;
            }
            let attribute_name = if namespace.is_empty() {
                attribute.local.to_string()
            } else {
                let prefix = self.prefix_for(
                    &mut bindings,
                    &mut declarations,
                    attribute.prefix.as_ref().map(|prefix| prefix.as_ref()),
                    namespace,
                );
                format!("{prefix}:{}", attribute.local)
            };
            attributes.push((attribute_name, value));
        }

        write!(self.writer, "<{qualified_name}")?;
        for (prefix, namespace) in declarations {
            let name = if prefix.is_empty() {
                "xmlns".to_owned()
            } else {
                format!("xmlns:{prefix}")
            };
            self.write_attribute(&name, &namespace)?;
        }
        for (name, value) in attributes {
            self.write_attribute(&name, value)?;
        }
        self.writer.write_all(b">")?;
        self.scopes.push(ElementScope {
            bindings,
            qualified_name,
        });
        Ok(())
    }

    fn end_elem(&mut self, _name: QualName) -> io::Result<()> {
        let scope = self.scopes.pop().expect("matching XML start tag");
        write!(self.writer, "</{}>", scope.qualified_name)
    }

    fn write_text(&mut self, text: &str) -> io::Result<()> {
        XmlSerializer::new(&mut self.writer).write_text(text)
    }

    fn write_comment(&mut self, text: &str) -> io::Result<()> {
        XmlSerializer::new(&mut self.writer).write_comment(text)
    }

    fn write_doctype(&mut self, name: &str) -> io::Result<()> {
        XmlSerializer::new(&mut self.writer).write_doctype(name)
    }

    fn write_processing_instruction(&mut self, target: &str, data: &str) -> io::Result<()> {
        XmlSerializer::new(&mut self.writer).write_processing_instruction(target, data)
    }
}
