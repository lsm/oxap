use std::collections::BTreeMap;

use oxap_adapter::gojson::{go_float, go_marshal};
use oxap_protocol::Usage;
use serde_json::value::RawValue;

use crate::native::{text, Object};

#[derive(Clone, Debug, Default)]
pub(crate) struct UsageState {
    input: Option<u64>,
    output: Option<u64>,
    pub(crate) measured: bool,
    cost: Option<f64>,
    model: String,
}

impl UsageState {
    pub(crate) fn observe(&mut self, data: &Object) {
        let first = !self.measured;
        self.input = accumulate(self.input, data.get("inputTokens").map(Box::as_ref), first);
        self.output = accumulate(
            self.output,
            data.get("outputTokens").map(Box::as_ref),
            first,
        );
        self.cost = accumulate_cost(self.cost, data.get("cost").map(Box::as_ref), first);
        self.measured = true;
        let model = text(data, "model");
        if !model.is_empty() {
            self.model = model;
        }
    }

    pub(crate) fn portable(&self) -> Usage {
        Usage {
            input_tokens: self.input,
            output_tokens: self.output,
            ..Usage::default()
        }
    }

    pub(crate) fn extensions(&self) -> BTreeMap<String, Box<RawValue>> {
        let mut extensions = BTreeMap::new();
        if !self.measured {
            return extensions;
        }
        if let Some(cost) = self.cost {
            let json = format!("{{\"premium_requests\":{}}}", go_float(cost));
            extensions.insert("com.github.copilot.cost".into(), raw(json));
        }
        if !self.model.is_empty() {
            let json = serde_json::json!({ "model": self.model }).to_string();
            extensions.insert(
                "com.github.copilot.routed_model".into(),
                raw(go_marshal(&json)),
            );
        }
        extensions
    }
}

fn raw(json: String) -> Box<RawValue> {
    RawValue::from_string(json).expect("usage extensions are valid JSON")
}

fn accumulate(previous: Option<u64>, raw: Option<&RawValue>, first: bool) -> Option<u64> {
    let digits = raw?.get();
    if !digits.bytes().all(|digit| digit.is_ascii_digit()) {
        return None;
    }
    let next: u64 = digits.parse().ok()?;
    if first {
        return Some(next);
    }
    next.checked_add(previous?)
}

fn accumulate_cost(previous: Option<f64>, raw: Option<&RawValue>, first: bool) -> Option<f64> {
    let mut next: f64 = raw?.get().parse().ok()?;
    if !first {
        next += previous?;
    }
    next.is_finite().then_some(next)
}
