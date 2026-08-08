// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! Reading inspectable text out of each provider's request schema, and writing it back.
//!
//! The three providers say the same thing three ways, so the gateway cannot inspect a request
//! without understanding whose schema it is. Rather than parse each into a common struct — which
//! would mean re-serialising and risking dropped fields the gateway does not know about — every
//! adapter returns **JSON pointers** to the text-bearing locations. The body is forwarded as the
//! caller wrote it, with only those locations rewritten when masking.

use serde_json::Value;

/// Which provider schema a request body follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Anthropic,
    OpenAi,
    Google,
}

impl Provider {
    /// Map a configured provider name to its schema.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "anthropic" => Some(Provider::Anthropic),
            "openai" => Some(Provider::OpenAi),
            "google" => Some(Provider::Google),
            _ => None,
        }
    }

    /// JSON pointers to every user-supplied text field in `body`, in document order.
    #[must_use]
    pub fn text_pointers(self, body: &Value) -> Vec<String> {
        match self {
            Provider::Anthropic => anthropic(body),
            Provider::OpenAi => openai(body),
            Provider::Google => google(body),
        }
    }
}

/// Anthropic Messages API: a `system` prompt plus `messages[].content`, where content is either a
/// bare string or an array of typed blocks.
fn anthropic(body: &Value) -> Vec<String> {
    let mut pointers = Vec::new();

    if body.get("system").and_then(Value::as_str).is_some() {
        pointers.push("/system".to_string());
    }
    // The system prompt may itself be an array of blocks.
    collect_blocks(body.get("system"), "/system", &mut pointers);

    if let Some(messages) = body.get("messages").and_then(Value::as_array) {
        for (index, message) in messages.iter().enumerate() {
            let base = format!("/messages/{index}/content");
            if message.get("content").and_then(Value::as_str).is_some() {
                pointers.push(base);
            } else {
                collect_blocks(message.get("content"), &base, &mut pointers);
            }
        }
    }
    pointers
}

/// OpenAI chat completions: `messages[].content`, a string or an array of parts.
fn openai(body: &Value) -> Vec<String> {
    let mut pointers = Vec::new();
    if let Some(messages) = body.get("messages").and_then(Value::as_array) {
        for (index, message) in messages.iter().enumerate() {
            let base = format!("/messages/{index}/content");
            if message.get("content").and_then(Value::as_str).is_some() {
                pointers.push(base);
            } else {
                collect_blocks(message.get("content"), &base, &mut pointers);
            }
        }
    }
    pointers
}

/// Google generateContent: `contents[].parts[].text`, plus an optional `systemInstruction`.
fn google(body: &Value) -> Vec<String> {
    let mut pointers = Vec::new();

    if let Some(parts) = body
        .get("systemInstruction")
        .and_then(|s| s.get("parts"))
        .and_then(Value::as_array)
    {
        for (part, _) in parts.iter().enumerate() {
            if parts[part].get("text").and_then(Value::as_str).is_some() {
                pointers.push(format!("/systemInstruction/parts/{part}/text"));
            }
        }
    }

    if let Some(contents) = body.get("contents").and_then(Value::as_array) {
        for (index, content) in contents.iter().enumerate() {
            let Some(parts) = content.get("parts").and_then(Value::as_array) else {
                continue;
            };
            for (part, value) in parts.iter().enumerate() {
                if value.get("text").and_then(Value::as_str).is_some() {
                    pointers.push(format!("/contents/{index}/parts/{part}/text"));
                }
            }
        }
    }
    pointers
}

/// Collect `text` fields from an array of typed content blocks.
///
/// Non-text blocks — images, tool results, documents — are skipped deliberately. Layer 1 reads
/// text; pretending to inspect a base64 image would be theatre.
fn collect_blocks(value: Option<&Value>, base: &str, pointers: &mut Vec<String>) {
    let Some(blocks) = value.and_then(Value::as_array) else {
        return;
    };
    for (index, block) in blocks.iter().enumerate() {
        if block.get("text").and_then(Value::as_str).is_some() {
            pointers.push(format!("{base}/{index}/text"));
        }
    }
}

/// Read the string at `pointer`, if it is a string.
#[must_use]
pub fn read_text<'a>(body: &'a Value, pointer: &str) -> Option<&'a str> {
    body.pointer(pointer).and_then(Value::as_str)
}

/// Replace the string at `pointer`. Returns false when the pointer does not address a string.
pub fn write_text(body: &mut Value, pointer: &str, replacement: String) -> bool {
    match body.pointer_mut(pointer) {
        Some(slot) if slot.is_string() => {
            *slot = Value::String(replacement);
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn anthropic_string_and_block_content_are_both_found() {
        let body = json!({
            "model": "claude-opus-5",
            "system": "You are helpful.",
            "messages": [
                {"role": "user", "content": "plain string"},
                {"role": "assistant", "content": [
                    {"type": "text", "text": "first block"},
                    {"type": "image", "source": {"data": "AAAA"}},
                    {"type": "text", "text": "second block"}
                ]}
            ]
        });
        let pointers = Provider::Anthropic.text_pointers(&body);
        assert_eq!(
            pointers,
            vec![
                "/system",
                "/messages/0/content",
                "/messages/1/content/0/text",
                "/messages/1/content/2/text",
            ]
        );
        // The image block is skipped, not silently treated as text.
        assert_eq!(
            read_text(&body, "/messages/1/content/0/text"),
            Some("first block")
        );
    }

    #[test]
    fn openai_multimodal_parts_are_found() {
        let body = json!({
            "messages": [
                {"role": "system", "content": "be terse"},
                {"role": "user", "content": [
                    {"type": "text", "text": "describe this"},
                    {"type": "image_url", "image_url": {"url": "https://example.com/a.png"}}
                ]}
            ]
        });
        assert_eq!(
            Provider::OpenAi.text_pointers(&body),
            vec!["/messages/0/content", "/messages/1/content/0/text"]
        );
    }

    #[test]
    fn google_contents_and_system_instruction_are_found() {
        let body = json!({
            "systemInstruction": {"parts": [{"text": "be terse"}]},
            "contents": [
                {"role": "user", "parts": [{"text": "hello"}, {"inlineData": {"data": "AAAA"}}]},
                {"role": "model", "parts": [{"text": "hi"}]}
            ]
        });
        assert_eq!(
            Provider::Google.text_pointers(&body),
            vec![
                "/systemInstruction/parts/0/text",
                "/contents/0/parts/0/text",
                "/contents/1/parts/0/text",
            ]
        );
    }

    #[test]
    fn writing_back_leaves_unrelated_fields_untouched() {
        let mut body = json!({
            "model": "gpt-4o",
            "temperature": 0.7,
            "vendor_extension": {"keep": "me"},
            "messages": [{"role": "user", "content": "secret"}]
        });
        assert!(write_text(
            &mut body,
            "/messages/0/content",
            "[MASKED]".into()
        ));

        assert_eq!(body["messages"][0]["content"], "[MASKED]");
        assert_eq!(body["temperature"], 0.7);
        assert_eq!(body["vendor_extension"]["keep"], "me");
    }

    #[test]
    fn malformed_bodies_yield_no_pointers_rather_than_panicking() {
        for body in [json!({}), json!({"messages": "not an array"}), json!(null)] {
            for provider in [Provider::Anthropic, Provider::OpenAi, Provider::Google] {
                assert!(provider.text_pointers(&body).is_empty(), "{body:?}");
            }
        }
    }
}
