use std::io;

pub(crate) struct AgentVersionRequirement {
    pub label: &'static str,
    pub binary: &'static str,
    pub args: &'static [&'static str],
    pub min_version: &'static str,
}

pub(crate) fn agent_version_requirement(
    target: crate::api::schema::IntegrationTarget,
) -> Option<AgentVersionRequirement> {
    match target {
        crate::api::schema::IntegrationTarget::Kimi => Some(AgentVersionRequirement {
            label: "kimi code",
            binary: "kimi",
            args: &["--version"],
            min_version: super::KIMI_MIN_VERSION,
        }),
        _ => None,
    }
}

pub(crate) fn extract_version_triple(text: &str) -> Option<(u64, u64, u64)> {
    text.split_whitespace().find_map(|token| {
        let token = token.trim_start_matches('v');
        let mut parts = token.splitn(3, '.');
        let major: u64 = parts.next()?.parse().ok()?;
        let minor: u64 = parts.next()?.parse().ok()?;
        let patch: u64 = parts
            .next()
            .map(|rest| {
                rest.chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect::<String>()
            })
            .and_then(|digits| digits.parse().ok())
            .unwrap_or(0);
        Some((major, minor, patch))
    })
}

/// Returns `Ok(None)` when the installed agent satisfies the requirement,
/// `Ok(Some(warning))` when the version cannot be determined (install
/// proceeds), and `Err` when the installed agent is too old.
pub(crate) fn enforce_agent_version(
    requirement: &AgentVersionRequirement,
) -> io::Result<Option<String>> {
    let probe = format!("{} {}", requirement.binary, requirement.args.join(" "));
    let output = match crate::noninteractive_process::command(requirement.binary)
        .args(requirement.args)
        .output()
    {
        Ok(output) if output.status.success() => output,
        _ => {
            return Ok(Some(format!(
                "{} could not run `{probe}` to verify the installed version; hooks require {} {} or newer",
                super::INSTALL_WARNING_PREFIX,
                requirement.label,
                requirement.min_version
            )));
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let Some(found) = extract_version_triple(&stdout) else {
        return Ok(Some(format!(
            "{} could not parse the {} version from `{probe}` output; hooks require {} {} or newer",
            super::INSTALL_WARNING_PREFIX,
            requirement.label,
            requirement.label,
            requirement.min_version
        )));
    };
    let required = extract_version_triple(requirement.min_version)
        .expect("static min version must be a valid version triple");

    if found < required {
        return Err(io::Error::other(format!(
            "{label} {}.{}.{} is too old: herdr hooks require {label} {min} or newer. upgrade {label}, then re-run install",
            found.0,
            found.1,
            found.2,
            label = requirement.label,
            min = requirement.min_version
        )));
    }
    Ok(None)
}
