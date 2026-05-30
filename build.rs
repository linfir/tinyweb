use std::{env, fmt::Write, fs, path::Path};

// ----------------------------------------------------------------------------

struct Mime<'a> {
    ext: &'a str,
    mime: &'a str,
    variant: &'a str,
}

fn parse_mime(src: &str) -> Vec<Mime<'_>> {
    let mut types = Vec::new();
    for line in src.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (ext, rest) = line
            .split_once(char::is_whitespace)
            .expect("Missing mime type");
        let rest = rest.trim();
        let (mime, variant) = rest
            .rsplit_once(char::is_whitespace)
            .expect("Missing variant");
        types.push(Mime {
            ext,
            mime: mime.trim_end(),
            variant,
        });
    }
    types
}

fn generate_mime(types: &[Mime], dst: &mut String) {
    dst.push('\n');
    dst.push_str("#[derive(Debug, Clone)]\n");
    dst.push_str("enum ContentTypeInner {\n");
    for t in types {
        let _ = writeln!(dst, "\t{},", t.variant);
    }
    dst.push_str("\tDefault,\n");
    dst.push_str("\tCustom(String),\n}\n\n");

    dst.push_str("impl ContentTypeInner {\n");
    dst.push_str("\tfn as_str(&self) -> &str {\n");
    dst.push_str("\t\tmatch self {\n");
    for t in types {
        let _ = writeln!(dst, "\t\t\tSelf::{} => \"{}\",", t.variant, t.mime);
    }
    dst.push_str("\t\t\tSelf::Default => \"application/octet-stream\",\n");
    dst.push_str("\t\t\tSelf::Custom(s) => s.as_str(),\n");
    dst.push_str("\t\t}\n\t}\n}\n\n");

    dst.push_str("impl ContentType {\n");
    dst.push_str(
        "\t/// Returns the content type for the given file extension (without leading dot),\n",
    );
    dst.push_str("\t/// or `None` if the extension is not recognised.\n");
    dst.push_str("\tpub fn from_extension(ext: Option<&str>) -> Option<Self> {\n");
    dst.push_str("\t\tmatch ext {\n");
    for t in types {
        let _ = writeln!(
            dst,
            "\t\t\tSome(\"{}\") => Some(Self(ContentTypeInner::{})),",
            t.ext, t.variant
        );
    }
    dst.push_str("\t\t\t_ => None,\n");
    dst.push_str("\t\t}\n\t}\n\n");

    for t in types {
        let constant = t.variant.to_ascii_uppercase();
        let _ = writeln!(dst, "\t/// The `{}` content type.", t.mime);
        let _ = writeln!(
            dst,
            "\tpub const {}: Self = ContentType(ContentTypeInner::{});",
            constant, t.variant
        );
    }
    dst.push_str("\t/// The default (`application/octet-stream`) content type.\n");
    dst.push_str("\tpub const DEFAULT: Self = ContentType(ContentTypeInner::Default);\n");
    dst.push_str("}\n");
}

// ----------------------------------------------------------------------------

fn parse_method(src: &str) -> Vec<&str> {
    src.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect()
}

fn generate_method(methods: &[&str], dst: &mut String) {
    dst.push('\n');
    dst.push_str("/// An HTTP request method.\n");
    dst.push_str("#[derive(Debug, Clone, Copy, PartialEq, Eq)]\n");
    dst.push_str("pub enum Method {\n");
    for m in methods {
        let _ = writeln!(dst, "\t{},", m);
    }
    dst.push_str("}\n\n");

    dst.push_str("impl Method {\n");
    dst.push_str("\t/// Parses an HTTP method from its byte representation.\n");
    dst.push_str("\tpub fn from_bytes(s: &[u8]) -> Option<Self> {\n");
    dst.push_str("\t\tmatch s {\n");
    for m in methods {
        let _ = writeln!(dst, "\t\t\tb\"{}\" => Some(Self::{}),", m, m);
    }
    dst.push_str("\t\t\t_ => None,\n");
    dst.push_str("\t\t}\n\t}\n\n");

    dst.push_str("\t/// Returns the string representation of the HTTP method.\n");
    dst.push_str("\tpub fn as_str(self) -> &'static str {\n");
    dst.push_str("\t\tmatch self {\n");
    for m in methods {
        let _ = writeln!(dst, "\t\t\tMethod::{} => \"{}\",", m, m);
    }
    dst.push_str("\t\t}\n\t}\n");
    dst.push_str("}\n");
}

// ----------------------------------------------------------------------------

struct StatusCode<'a> {
    code: u16,
    variant: String,
    text: &'a str,
}

fn parse_status_code_line(line: &str) -> Option<StatusCode<'_>> {
    let (a, b) = line.split_once(' ')?;
    let code = a.parse().ok()?;
    let text = b.trim();
    let variant = if text == "OK" {
        "Ok".to_string()
    } else {
        text.replace(' ', "")
    };

    Some(StatusCode {
        code,
        variant,
        text,
    })
}

fn parse_status_code(src: &str) -> Vec<StatusCode<'_>> {
    let mut v = Vec::new();
    for line in src.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        v.push(parse_status_code_line(line).expect("Failed to parse status code"));
    }
    v
}

fn generate_status_code(codes: &[StatusCode<'_>], dst: &mut String) {
    dst.push('\n');
    dst.push_str("/// An HTTP status code.\n");
    dst.push_str("#[derive(Debug, Clone, Copy, PartialEq, Eq)]\n");
    dst.push_str("pub enum StatusCode {\n");
    for c in codes {
        let _ = writeln!(dst, "\t/// HTTP/1.1 {} {}", c.code, c.text);
        let _ = writeln!(dst, "\t{},", c.variant);
    }
    dst.push_str("}\n\n");

    dst.push_str("impl StatusCode {\n");
    dst.push_str("\t/// Returns the numeric status code.\n");
    dst.push_str("\tpub fn as_u16(self) -> u16 {\n");
    dst.push_str("\t\tmatch self {\n");
    for c in codes {
        let _ = writeln!(dst, "\t\t\tSelf::{} => {},", c.variant, c.code);
    }
    dst.push_str("\t\t}\n\t}\n\n");

    dst.push_str("\t/// Returns the reason phrase (e.g. `\"OK\"`, `\"Not Found\"`).\n");
    dst.push_str("\tpub fn as_str(self) -> &'static str {\n");
    dst.push_str("\t\tmatch self {\n");
    for c in codes {
        let _ = writeln!(dst, "\t\t\tSelf::{} => \"{}\",", c.variant, c.text);
    }
    dst.push_str("\t\t}\n\t}\n}\n");
}

// ----------------------------------------------------------------------------

struct HeaderNameEntry<'a>(&'a str);

impl<'a> HeaderNameEntry<'a> {
    fn header(&self) -> &'a str {
        self.0
    }

    fn variant(&self) -> String {
        self.0.replace('-', "")
    }

    fn constant(&self) -> String {
        self.0.to_ascii_uppercase().replace('-', "_")
    }
}

fn parse_header_name(src: &str) -> Vec<HeaderNameEntry<'_>> {
    let mut entries = Vec::new();
    for line in src.lines() {
        let line = line.trim();
        if !line.is_empty() {
            entries.push(HeaderNameEntry(line));
        }
    }
    entries
}

fn generate_header_name(entries: &[HeaderNameEntry], dst: &mut String) {
    dst.push('\n');
    dst.push_str("#[derive(Debug, Clone, PartialEq, Eq)]\n");
    dst.push_str("enum HeaderNameInner {\n");
    for e in entries {
        let _ = writeln!(dst, "\t{},", e.variant());
    }
    dst.push_str("\tCustom(String),\n}\n\n");

    dst.push_str("impl HeaderNameInner {\n");
    dst.push_str("\tfn as_str(&self) -> &str {\n");
    dst.push_str("\t\tmatch self {\n");
    for e in entries {
        let _ = writeln!(dst, "\t\t\tSelf::{} => \"{}\",", e.variant(), e.header());
    }
    dst.push_str("\t\t\tSelf::Custom(s) => s.as_str(),\n");
    dst.push_str("\t\t}\n\t}\n}\n\n");

    dst.push_str("impl HeaderName {\n");
    for e in entries {
        let _ = writeln!(dst, "\t/// The `{}` header.", e.header());
        let _ = writeln!(
            dst,
            "\tpub const {}: Self = HeaderName(HeaderNameInner::{});",
            e.constant(),
            e.variant()
        );
    }
    dst.push_str("}\n");
}

// ----------------------------------------------------------------------------

fn slurp(path: &str) -> String {
    println!("cargo:rerun-if-changed={path}");
    fs::read_to_string(path).unwrap_or_else(|_| panic!("Cannot open `{path}`"))
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    let out_dir = env::var("OUT_DIR").expect("OUT_DIR not set");
    let output_path = Path::new(&out_dir).join("generated.rs");

    let mut dst = String::from("// This file is @generated by `build.rs`\n");

    {
        let src = slurp("method.txt");
        let methods = parse_method(&src);
        generate_method(&methods, &mut dst);
    }

    {
        let src = slurp("mime.txt");
        let types = parse_mime(&src);
        generate_mime(&types, &mut dst);
    }

    {
        let src = slurp("status_code.txt");
        let codes = parse_status_code(&src);
        generate_status_code(&codes, &mut dst);
    }

    {
        let src = slurp("header_name.txt");
        let entries = parse_header_name(&src);
        generate_header_name(&entries, &mut dst);
    }

    dst = dst.replace('\t', "    ");
    fs::write(&output_path, dst).expect("Failed to write generated code");
}
