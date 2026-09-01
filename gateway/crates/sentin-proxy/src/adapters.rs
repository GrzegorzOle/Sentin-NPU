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
    /// Anthropic Messages API: text lives under `messages[].content`, as a string or as blocks.
    Anthropic,
    /// OpenAI chat completions, and everything speaking that dialect — Ollama, LM Studio, vLLM,
    /// and routers such as LiteLLM.
    OpenAi,
    /// Google Generative Language API, whose text sits under `contents[].parts[].text`.
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

/// One attachment found in a request body: where its bytes are, and what the caller called it.
///
/// The declared type is recorded but not trusted. What the bytes are is decided by looking at
/// them, because the sender is precisely the party whose data the gateway is trying not to leak.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    /// JSON pointer to the string holding base64, possibly wrapped in a `data:` URI.
    pub pointer: String,
    /// The media type the caller declared, when it declared one.
    pub declared_type: Option<String>,
}

impl Provider {
    /// Every attachment in `body`, in document order.
    ///
    /// The three providers carry a file three ways, and a gateway that understands one of them
    /// protects one client. Measured on 2026-09-01: a PDF holding a checksum-valid PESEL passed
    /// through as `findings=clean`, because base64 hides the digits from anything scanning the
    /// request body.
    #[must_use]
    pub fn attachments(self, body: &Value) -> Vec<Attachment> {
        let mut found = Vec::new();
        match self {
            // Anthropic: content blocks of type `document` or `image` with a base64 source.
            Provider::Anthropic => {
                walk_message_blocks(
                    body,
                    "messages",
                    &mut |base, block| {
                        let source = block.get("source")?;
                        if source.get("type").and_then(Value::as_str) != Some("base64") {
                            return None;
                        }
                        source.get("data").and_then(Value::as_str)?;
                        Some(Attachment {
                            pointer: format!("{base}/source/data"),
                            declared_type: source
                                .get("media_type")
                                .and_then(Value::as_str)
                                .map(ToString::to_string),
                        })
                    },
                    &mut found,
                );
            }
            // OpenAI dialect: `file.file_data` and `image_url.url`, both carrying a data URI.
            Provider::OpenAi => {
                walk_message_blocks(
                    body,
                    "messages",
                    &mut |base, block| {
                        if let Some(file) = block.get("file") {
                            if file.get("file_data").and_then(Value::as_str).is_some() {
                                return Some(Attachment {
                                    pointer: format!("{base}/file/file_data"),
                                    declared_type: None,
                                });
                            }
                        }
                        let url = block.get("image_url")?.get("url")?.as_str()?;
                        // A hosted image is a URL the gateway does not fetch: reaching out to it would
                        // turn an inspection point into a request forger.
                        url.starts_with("data:").then(|| Attachment {
                            pointer: format!("{base}/image_url/url"),
                            declared_type: None,
                        })
                    },
                    &mut found,
                );
            }
            // Google: `inline_data.data`, beside the text parts.
            Provider::Google => {
                if let Some(contents) = body.get("contents").and_then(Value::as_array) {
                    for (index, content) in contents.iter().enumerate() {
                        let Some(parts) = content.get("parts").and_then(Value::as_array) else {
                            continue;
                        };
                        for (part, value) in parts.iter().enumerate() {
                            let Some(inline) =
                                value.get("inline_data").or_else(|| value.get("inlineData"))
                            else {
                                continue;
                            };
                            if inline.get("data").and_then(Value::as_str).is_none() {
                                continue;
                            }
                            let key = if value.get("inline_data").is_some() {
                                "inline_data"
                            } else {
                                "inlineData"
                            };
                            found.push(Attachment {
                                pointer: format!("/contents/{index}/parts/{part}/{key}/data"),
                                declared_type: inline
                                    .get("mime_type")
                                    .or_else(|| inline.get("mimeType"))
                                    .and_then(Value::as_str)
                                    .map(ToString::to_string),
                            });
                        }
                    }
                }
            }
        }
        found
    }
}

/// Walk `messages[].content[]`, offering each block to `pick`.
fn walk_message_blocks(
    body: &Value,
    field: &str,
    pick: &mut dyn FnMut(&str, &Value) -> Option<Attachment>,
    found: &mut Vec<Attachment>,
) {
    let Some(messages) = body.get(field).and_then(Value::as_array) else {
        return;
    };
    for (index, message) in messages.iter().enumerate() {
        let Some(blocks) = message.get("content").and_then(Value::as_array) else {
            continue;
        };
        for (block_index, block) in blocks.iter().enumerate() {
            let base = format!("/{field}/{index}/content/{block_index}");
            if let Some(attachment) = pick(&base, block) {
                found.push(attachment);
            }
        }
    }
}

/// Collect `text` fields from an array of typed content blocks.
///
/// Non-text blocks are skipped here and picked up by [`Provider::attachments`] instead, which
/// decodes them and reads what is inside. Scanning the base64 itself would be theatre: the bytes
/// `87031406724` are in the document and absent from its encoding.
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

    #[test]
    fn anthropic_documents_and_images_are_found_by_their_base64_source() {
        let body = serde_json::json!({"messages":[{"role":"user","content":[
            {"type":"text","text":"Streszcz to"},
            {"type":"document","source":{"type":"base64","media_type":"application/pdf","data":"JVBERi0="}},
            {"type":"image","source":{"type":"base64","media_type":"image/png","data":"iVBOR"}}]}]});
        let found = Provider::Anthropic.attachments(&body);
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0].pointer, "/messages/0/content/1/source/data");
        assert_eq!(found[0].declared_type.as_deref(), Some("application/pdf"));
    }

    #[test]
    fn a_url_source_is_not_an_attachment_this_gateway_will_fetch() {
        // Reaching out to a URL a caller supplied would turn an inspection point into a request
        // forger, and the bytes would never have been on this machine in the first place.
        let body = serde_json::json!({"messages":[{"role":"user","content":[
            {"type":"image","source":{"type":"url","url":"https://example.invalid/x.png"}}]}]});
        assert!(Provider::Anthropic.attachments(&body).is_empty());

        let body = serde_json::json!({"messages":[{"role":"user","content":[
            {"type":"image_url","image_url":{"url":"https://example.invalid/x.png"}}]}]});
        assert!(Provider::OpenAi.attachments(&body).is_empty());
    }

    #[test]
    fn the_openai_dialect_carries_a_file_two_ways() {
        let body = serde_json::json!({"messages":[{"role":"user","content":[
            {"type":"file","file":{"filename":"umowa.pdf","file_data":"data:application/pdf;base64,JVBERi0="}},
            {"type":"image_url","image_url":{"url":"data:image/png;base64,iVBOR"}}]}]});
        let found = Provider::OpenAi.attachments(&body);
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0].pointer, "/messages/0/content/0/file/file_data");
        assert_eq!(found[1].pointer, "/messages/0/content/1/image_url/url");
    }

    #[test]
    fn google_inline_data_is_found_under_either_spelling() {
        // The REST API uses snake_case and the client libraries emit camelCase; a gateway that
        // understands one of them protects half the callers.
        for key in ["inline_data", "inlineData"] {
            let body = serde_json::json!({"contents":[{"parts":[
                {"text":"Streszcz"},
                {key:{"mime_type":"application/pdf","data":"JVBERi0="}}]}]});
            let found = Provider::Google.attachments(&body);
            assert_eq!(found.len(), 1, "{key}: {found:?}");
            assert_eq!(found[0].pointer, format!("/contents/0/parts/1/{key}/data"));
        }
    }

    #[test]
    fn a_request_with_no_attachments_finds_none() {
        let body = serde_json::json!({"messages":[{"role":"user","content":"zwykly tekst"}]});
        assert!(Provider::OpenAi.attachments(&body).is_empty());
        assert!(Provider::Anthropic.attachments(&body).is_empty());
    }
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
