use oxap_adapter::Error;
use url::Url;

const BASE_URL: &str = "ANTHROPIC_BASE_URL";
const API_KEY: &str = "ANTHROPIC_API_KEY";
const AUTH_TOKEN: &str = "ANTHROPIC_AUTH_TOKEN";
const PROVIDER_VARIABLES: [&str; 4] = [BASE_URL, API_KEY, AUTH_TOKEN, "CLAUDE_CODE_OAUTH_TOKEN"];
const CUSTOM_HEADERS: &str = "ANTHROPIC_CUSTOM_HEADERS";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthMode {
    None,
    ApiKey,
    Token,
}

#[derive(Clone)]
pub struct Provider {
    pub base_url: String,
    pub auth: AuthMode,
    pub secret: String,
}

#[derive(Clone)]
pub struct Header {
    pub name: String,
    pub value: String,
}

pub(crate) fn configure_environment(
    environment: &mut Vec<String>,
    provider: Option<Provider>,
    headers: &[Header],
) -> Result<(), Error> {
    if let Some(provider) = provider {
        if !valid_url(&provider.base_url) {
            return Err(refused("provider base URL is not an http or https URL"));
        }
        let (variable, secret) = credential(&provider)?;
        environment.retain(|entry| {
            let name = entry
                .split_once('=')
                .map_or(entry.as_str(), |(name, _)| name);
            !PROVIDER_VARIABLES.contains(&name)
        });
        environment.push(format!("{BASE_URL}={}", provider.base_url));
        environment.push(format!("{variable}={secret}"));
    }
    if !headers.is_empty() {
        append_headers(environment, headers)?;
    }
    if environment.iter().any(|entry| entry.contains('\0')) {
        return Err(refused("environment entry carries NUL"));
    }
    Ok(())
}

fn valid_url(value: &str) -> bool {
    let Some((scheme, rest)) = value.split_once("://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    (scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https"))
        && !authority.is_empty()
        && !authority.contains([' ', '\\'])
        && !value.bytes().any(|byte| byte.is_ascii_control())
        && Url::parse(value).is_ok_and(|url| url.has_host())
}

fn credential(provider: &Provider) -> Result<(&'static str, &str), Error> {
    let secret = provider.secret.as_str();
    if secret.contains(['\r', '\n', '\0']) {
        return Err(refused("provider secret carries a line break or NUL"));
    }
    match provider.auth {
        AuthMode::None if !secret.is_empty() => {
            Err(refused("a provider with auth none takes no secret"))
        }
        AuthMode::None => Ok((API_KEY, "oap-unauthenticated")),
        AuthMode::ApiKey if secret.is_empty() => {
            Err(refused("a provider with auth api_key needs a secret"))
        }
        AuthMode::Token if secret.is_empty() => {
            Err(refused("a provider with auth token needs a secret"))
        }
        AuthMode::ApiKey => Ok((API_KEY, secret)),
        AuthMode::Token => Ok((AUTH_TOKEN, secret)),
    }
}

fn append_headers(environment: &mut Vec<String>, headers: &[Header]) -> Result<(), Error> {
    let mut lines = Vec::with_capacity(headers.len());
    for Header { name, value } in headers {
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
            || value.contains(['\r', '\n', '\0'])
        {
            return Err(refused(&format!(
                "header {name:?} cannot be carried in {CUSTOM_HEADERS}"
            )));
        }
        lines.push(format!("{name}: {value}"));
    }
    let joined = lines.join("\n");
    let prefix = "ANTHROPIC_CUSTOM_HEADERS=";
    if let Some(entry) = environment
        .iter_mut()
        .find(|entry| entry.starts_with(prefix))
    {
        if entry.len() > prefix.len() {
            entry.push('\n');
        }
        entry.push_str(&joined);
    } else {
        environment.push(format!("{prefix}{joined}"));
    }
    Ok(())
}

fn refused(detail: &str) -> Error {
    Error::Harness(format!("claude adapter: {detail}"))
}
