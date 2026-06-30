const API_SCHEMA_JSON: &str = include_str!("../../docs/next/api/herdr-api.schema.json");

pub(super) fn run_api_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(String::as_str) else {
        print_api_help();
        return Ok(2);
    };

    match subcommand {
        "schema" => api_schema(&args[1..]),
        "help" | "--help" | "-h" => {
            print_api_help();
            Ok(0)
        }
        _ => {
            print_api_help();
            Ok(2)
        }
    }
}

fn api_schema(args: &[String]) -> std::io::Result<i32> {
    match args {
        [] => {
            print!("{}", schema_summary_text()?);
        }
        [flag] if flag == "--json" => {
            print!("{API_SCHEMA_JSON}");
        }
        [flag, path] if flag == "--output" => {
            write_schema_file(std::path::Path::new(path))?;
            println!("wrote API schema to {path}");
        }
        [flag] if flag == "--output" => {
            eprintln!("missing value for --output");
            return Ok(2);
        }
        [flag] if matches!(flag.as_str(), "help" | "--help" | "-h") => {
            print_api_schema_help();
        }
        [other] if other.starts_with('-') => {
            eprintln!("unknown option: {other}");
            return Ok(2);
        }
        _ => {
            print_api_schema_help();
            return Ok(2);
        }
    }
    Ok(0)
}

fn write_schema_file(path: &std::path::Path) -> std::io::Result<()> {
    std::fs::write(path, API_SCHEMA_JSON)
}

fn schema_summary_text() -> std::io::Result<String> {
    let value: serde_json::Value = serde_json::from_str(API_SCHEMA_JSON)?;
    let protocol = value
        .get("protocol")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| std::io::Error::other("API schema is missing protocol"))?;
    let schema_version = value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| std::io::Error::other("API schema is missing schema_version"))?;
    let mut schemas: Vec<&str> = value
        .get("schemas")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| std::io::Error::other("API schema is missing schemas"))?
        .keys()
        .map(String::as_str)
        .collect();
    schemas.sort();

    Ok(format!(
        "Herdr API schema\nprotocol: {}\nschema_version: {}\nschemas: {}\n\nUse `herdr api schema --json` to print the full schema.\nUse `herdr api schema --output PATH` to write it to a file.\n",
        protocol,
        schema_version,
        schemas.join(", ")
    ))
}

fn print_api_help() {
    eprintln!("herdr api commands:");
    eprintln!("  herdr api schema [--json | --output PATH]");
}

fn print_api_schema_help() {
    eprintln!("usage: herdr api schema [--json | --output PATH]");
}

#[cfg(test)]
mod tests {
    #[test]
    fn schema_summary_text_stays_human_sized() {
        let text = super::schema_summary_text().unwrap();
        assert!(text.contains("Herdr API schema"));
        assert!(text.contains("Use `herdr api schema --json`"));
        assert!(text.len() < 400);
    }
}
