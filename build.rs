use std::{env, fmt::Write, fs, io, path::Path};

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
        let mut parts = line.split_whitespace();
        let ext = parts.next().expect("Missing extension");
        let mime = parts.next().expect("Missing mime type");
        let variant = parts.next().expect("Missing variant name");
        types.push(Mime { ext, mime, variant });
    }
    types
}

fn generate_mime(types: &[Mime], dst: &mut String) {
    dst.push('\n');
    dst.push_str("/// A content type (MIME type) for HTTP responses.\n");
    dst.push_str("#[derive(Debug, Clone, Copy, PartialEq, Eq)]\n");
    dst.push_str("pub enum ContentType {\n");
    for t in types {
        let _ = writeln!(dst, "\t{},", t.variant);
    }
    dst.push_str("\tDefault,\n}\n\n");

    dst.push_str("impl ContentType {\n");
    dst.push_str(
        "\t/// Returns the content type for the given file extension (without leading dot).\n",
    );
    dst.push_str("\tpub fn from_extension(ext: Option<&str>) -> Self {\n");
    dst.push_str("\t\tmatch ext {\n");
    for t in types {
        let _ = writeln!(dst, "\t\t\tSome(\"{}\") => Self::{},", t.ext, t.variant);
    }
    dst.push_str("\t\t\t_ => Self::Default,\n");
    dst.push_str("\t\t}\n\t}\n\n");

    dst.push_str("\t/// Returns the MIME type string (e.g. `\"text/html\"`).\n");
    dst.push_str("\tpub fn as_str(self) -> &'static str {\n");
    dst.push_str("\t\tmatch self {\n");
    for t in types {
        let _ = writeln!(dst, "\t\t\tSelf::{} => \"{}\",", t.variant, t.mime);
    }
    dst.push_str("\t\t\tSelf::Default => \"application/octet-stream\",\n");
    dst.push_str("\t\t}\n\t}\n}\n");
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
    dst.push_str("\t\t}\n\t}\n}\n");
}

// ----------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct MySplitWhitespace<'a> {
    rest: &'a str,
}

impl<'a> MySplitWhitespace<'a> {
    fn new(s: &'a str) -> Self {
        MySplitWhitespace { rest: s.trim() }
    }

    fn rest(&self) -> &'a str {
        self.rest
    }
}

impl<'a> Iterator for MySplitWhitespace<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        if self.rest.is_empty() {
            return None;
        }
        match self.rest.find(char::is_whitespace) {
            Some(i) => {
                let (a, b) = self.rest.split_at(i);
                self.rest = b.trim_start();
                Some(a)
            }
            None => {
                let a = self.rest;
                self.rest = "";
                Some(a)
            }
        }
    }
}

// ----------------------------------------------------------------------------

struct StatusCode<'a> {
    code: u16,
    variant: &'a str,
    text: &'a str,
}

fn parse_status_code_line(line: &str) -> Option<StatusCode<'_>> {
    let mut parts = MySplitWhitespace::new(line);
    let code = parts.next()?.parse().ok()?;
    let variant = parts.next()?;
    let text = parts.rest();

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

fn generate_status_code(codes: &[StatusCode], dst: &mut String) {
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

fn main() -> io::Result<()> {
    println!("cargo:rerun-if-changed=build.rs");

    let input = Path::new("mime.txt");
    let out_dir = env::var("OUT_DIR").expect("OUT_DIR not set");
    let output_path = Path::new(&out_dir).join("generated.rs");

    let mut dst = String::from("// This file is @generated by `build.rs`\n");

    {
        println!("cargo:rerun-if-changed=method.txt");
        let src = fs::read_to_string("method.txt").expect("Cannot open `method.txt`");
        let methods = parse_method(&src);
        generate_method(&methods, &mut dst);
    }

    {
        println!("cargo:rerun-if-changed=mime.txt");
        let src = fs::read_to_string(input).expect("Cannot open `mime.txt`");
        let types = parse_mime(&src);
        generate_mime(&types, &mut dst);
    }

    {
        println!("cargo:rerun-if-changed=status_code.txt");
        let src = fs::read_to_string("status_code.txt").expect("Cannot open `status_code.txt`");
        let codes = parse_status_code(&src);
        generate_status_code(&codes, &mut dst);
    }

    dst = dst.replace('\t', "    ");
    fs::write(&output_path, dst).expect("Failed to write generated code");

    Ok(())
}
