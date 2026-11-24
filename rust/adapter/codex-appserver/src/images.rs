use std::collections::HashSet;
use std::time::{Duration, Instant};

use base64::{engine::general_purpose::STANDARD, Engine};
use oxap_adapter::{Error, UnsupportedControl, CONTROL_UNSATISFIABLE};
use oxap_protocol::{ImageContent, MessageContent, FEATURE_CONTENT_IMAGE};
use serde_json::{json, Value};

use crate::client::ClientFactory;
use crate::native::UserInput;

const MAX_DECODED_IMAGE: usize = 4 << 20;
const MAX_ENCODED_IMAGES: usize = 6 << 20;
pub(crate) const MAX_IMAGE_TURN_FRAME: usize = 7 << 20;

#[derive(Default)]
pub(crate) struct ImageModels {
    ids: HashSet<String>,
    pub(crate) default_model: String,
}

impl ImageModels {
    pub(crate) fn discover(factory: &dyn ClientFactory, model: &str, cwd: &str) -> Self {
        let mut catalog = Self::default();
        let deadline = Some(Instant::now() + Duration::from_secs(5));
        let Ok(client) = factory.start(deadline, &crossbeam_channel::never()) else {
            return catalog;
        };
        let call = |method, params: Value| {
            let params = serde_json::value::to_raw_value(&params).ok()?;
            let reply = client.call(method, params, deadline).ok()?;
            serde_json::from_str::<Value>(reply.get()).ok()
        };
        let mut params = json!({"includeHidden": true});
        for _ in 0..4 {
            let Some(page) = call("model/list", params.clone()) else {
                break;
            };
            let Some(entries) = page["data"].as_array() else {
                break;
            };
            for entry in entries {
                let id = text(&entry["id"]);
                let model = text(&entry["model"]);
                if !id.is_empty()
                    && !model.is_empty()
                    && entry["inputModalities"]
                        .as_array()
                        .is_some_and(|modes| modes.iter().any(|mode| mode == "image"))
                {
                    catalog.ids.extend([id.to_owned(), model.to_owned()]);
                    if entry["isDefault"] == true {
                        catalog.default_model = model.to_owned();
                    }
                }
            }
            let cursor = text(&page["nextCursor"]);
            if cursor.is_empty() {
                break;
            }
            params["cursor"] = json!(cursor);
        }
        if model.is_empty() && !catalog.ids.is_empty() {
            let params = if cwd.is_empty() {
                json!({})
            } else {
                json!({"cwd": cwd})
            };
            let model = call("config/read", params)
                .map(|config| text(&config["config"]["model"]).to_owned());
            match model.as_deref() {
                Some("") => (),
                Some(model) => catalog.default_model = model.to_owned(),
                None => catalog.default_model.clear(),
            }
        }
        let _ = client.close();
        catalog
    }

    pub(crate) fn supports(&self, model: &str) -> bool {
        self.ids.contains(model)
    }
}

fn text(value: &Value) -> &str {
    value.as_str().unwrap_or_default()
}

fn inline_image(image: ImageContent, total: &mut usize) -> Result<UserInput, Error> {
    let reject = Error::InvalidSubmissionDetail;
    let ImageContent {
        media_type,
        data,
        url,
    } = image;
    if !matches!(
        media_type.as_str(),
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    ) {
        return Err(reject(
            "inline images require PNG, JPEG, GIF or WebP media_type",
        ));
    }
    if !url.is_empty() || data.is_empty() {
        return Err(reject("image input requires inline base64 data"));
    }
    if data.len() > MAX_ENCODED_IMAGES - *total {
        return Err(reject(
            "inline images exceed 6 MiB of base64 per submission",
        ));
    }
    let invalid = reject("image input requires valid standard base64");
    if data.len() % 4 != 0 {
        return Err(invalid);
    }
    let size = data.len() / 4 * 3 - (data.len() - data.trim_end_matches('=').len()).min(2);
    if size > MAX_DECODED_IMAGE {
        return Err(reject("an inline image exceeds 4 MiB decoded"));
    }
    STANDARD.decode(&data).map_err(|_| invalid)?;
    *total += data.len();
    Ok(UserInput {
        kind: "image".into(),
        url: format!("data:{media_type};base64,{data}"),
        ..UserInput::default()
    })
}

pub(crate) fn append_content(
    input: &mut Vec<UserInput>,
    content: &MessageContent,
    total: &mut usize,
) -> Result<(), Error> {
    let as_text = |text| UserInput {
        kind: "text".into(),
        text,
        ..UserInput::default()
    };
    if let Some(text) = content
        .raw()
        .and_then(|raw| serde_json::from_str::<Option<String>>(raw.get()).ok())
    {
        input.push(as_text(text.unwrap_or_default()));
        return Ok(());
    }
    let parts = content
        .parts()
        .filter(|parts| !parts.is_empty())
        .ok_or(Error::UnsupportedInput)?;
    for part in parts {
        let native = match part.part_type.as_str() {
            "text" => as_text(part.text),
            "image" => inline_image(part.image.unwrap_or_default(), total)?,
            _ => return Err(Error::InvalidSubmission),
        };
        input.push(native);
    }
    Ok(())
}

pub(crate) fn unsupported_image() -> Error {
    Error::UnsupportedControl(Box::new(UnsupportedControl {
        feature: FEATURE_CONTENT_IMAGE.into(),
        reason: CONTROL_UNSATISFIABLE,
        detail: "the effective Codex model does not support inline images".into(),
        ..UnsupportedControl::default()
    }))
}
