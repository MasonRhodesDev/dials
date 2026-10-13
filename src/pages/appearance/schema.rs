//! lmtt's config schema (the JSON lmtt-config renders in the terminal),
//! reduced to the rows this page draws. Lenient: unknown kinds become text,
//! unknown option sources become free text with the current value.
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Enum,
    Bool,
    Number,
    Float,
    Path,
    Text,
}

impl Kind {
    /// The Slint-side kind the page switches on.
    pub fn tag(self) -> &'static str {
        match self {
            Kind::Enum => "enum",
            Kind::Bool => "bool",
            Kind::Number | Kind::Float => "number",
            Kind::Path => "path",
            Kind::Text => "text",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Options {
    None,
    Static(Vec<String>),
    /// Named option provider lmtt-config registers (lmtt_gtk_themes, …).
    Provider(String),
}

#[derive(Debug, Clone)]
pub struct Field {
    /// TOML table path, e.g. `general` or `theme_profiles.light`.
    pub section: String,
    pub section_title: String,
    pub subsection: String,
    /// Key inside the section.
    pub id: String,
    pub label: String,
    pub description: String,
    pub kind: Kind,
    pub default: Value,
    pub options: Options,
}

impl Field {
    /// `<section>.<key>`: what the page uses to address a row.
    pub fn key(&self) -> String {
        format!("{}.{}", self.section, self.id)
    }
}

pub fn parse(text: &str) -> Result<Vec<Field>, String> {
    let root: Value = serde_json::from_str(text).map_err(|e| format!("schema: {e}"))?;
    let sections = root
        .get("sections")
        .and_then(Value::as_array)
        .ok_or("schema: no sections")?;
    let mut out = Vec::new();
    for section in sections {
        let sid = section
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if sid.is_empty() {
            continue;
        }
        let title = section
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or(&sid)
            .to_string();
        let Some(fields) = section.get("fields").and_then(Value::as_array) else {
            continue;
        };
        for f in fields {
            let id = f
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if id.is_empty() {
                continue;
            }
            let kind = match f.get("type").and_then(Value::as_str).unwrap_or("string") {
                "enum" => Kind::Enum,
                "boolean" => Kind::Bool,
                "number" | "integer" => Kind::Number,
                "float" => Kind::Float,
                "path" => Kind::Path,
                _ => Kind::Text,
            };
            let options = match f.get("options_source") {
                Some(src) => match src.get("type").and_then(Value::as_str) {
                    Some("static") => Options::Static(
                        src.get("values")
                            .and_then(Value::as_array)
                            .map(|v| {
                                v.iter()
                                    .filter_map(Value::as_str)
                                    .map(str::to_string)
                                    .collect()
                            })
                            .unwrap_or_default(),
                    ),
                    Some("provider") => src
                        .get("provider")
                        .and_then(Value::as_str)
                        .map(|p| Options::Provider(p.to_string()))
                        .unwrap_or(Options::None),
                    _ => Options::None,
                },
                None => Options::None,
            };
            out.push(Field {
                section: sid.clone(),
                section_title: title.clone(),
                subsection: f
                    .get("subsection")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                id: id.clone(),
                label: f
                    .get("label")
                    .and_then(Value::as_str)
                    .unwrap_or(&id)
                    .to_string(),
                description: f
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                kind,
                default: f.get("default").cloned().unwrap_or(Value::Null),
                options,
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: &str = r#"{"version":"1.0","sections":[
      {"id":"general","title":"General Settings","fields":[
        {"id":"default_mode","label":"Default Theme Mode","type":"enum","options_source":{"type":"static","values":["light","dark"]},"default":"dark","subsection":"Basic"},
        {"id":"use_matugen","label":"Use Matugen","type":"boolean","default":true,"subsection":"Basic"},
        {"id":"timeout","label":"Timeout","type":"number","default":10,"min":1,"max":60,"subsection":"Performance"},
        {"id":"wallpaper","label":"Wallpaper Path","type":"path","default":"","subsection":"Matugen"}]},
      {"id":"theme_profiles.dark","title":"Dark Theme Profile","fields":[
        {"id":"gtk_theme","label":"GTK Theme","type":"enum","options_source":{"type":"provider","provider":"lmtt_gtk_themes"},"default":"Adwaita-dark"},
        {"id":"terminal_opacity","label":"Opacity","type":"float","default":0.95}]}]}"#;

    #[test]
    fn parses_kinds_sections_and_option_sources() {
        let fields = parse(SCHEMA).unwrap();
        assert_eq!(fields.len(), 6);
        let f = &fields[0];
        assert_eq!(f.key(), "general.default_mode");
        assert_eq!(f.section_title, "General Settings");
        assert_eq!(f.subsection, "Basic");
        assert_eq!(f.kind, Kind::Enum);
        assert_eq!(
            f.options,
            Options::Static(vec!["light".into(), "dark".into()])
        );
        assert_eq!(fields[1].kind, Kind::Bool);
        assert_eq!(fields[2].kind, Kind::Number);
        assert_eq!(fields[3].kind, Kind::Path);
        let g = &fields[4];
        assert_eq!(g.section, "theme_profiles.dark");
        assert_eq!(g.options, Options::Provider("lmtt_gtk_themes".into()));
        assert_eq!(g.default, Value::String("Adwaita-dark".into()));
        assert_eq!(fields[5].kind, Kind::Float);
        assert_eq!(fields[5].kind.tag(), "number");
    }

    #[test]
    fn rejects_a_schema_without_sections() {
        assert!(parse("{}").is_err());
        assert!(parse("not json").is_err());
    }
}
