// Copyright 2026 Grzegorz Oleksy
// SPDX-License-Identifier: Apache-2.0

//! Layer 2: named-entity recognition over an OpenVINO IR model.
//!
//! `tools/validate_model.py` is the reference implementation and this must reproduce it exactly,
//! because the quality numbers in `docs/benchmarks.md` were measured there. Three details carry
//! that burden, and each of them was a bug in the Python version first:
//!
//! 1. **The label comes from a word's first subword, the character extent from all of them.**
//!    Conflating the two truncates entities mid-word — "Marka Wiśniowieckiego" collapses to
//!    "Marka Wiśni" — and cost 63 F1 points before it was spotted.
//! 2. **Spans must end up as byte offsets**, because [`Finding::span`] indexes a Rust `str` and
//!    layer 1 already produces byte ranges. The `tokenizers` crate reports byte offsets, so no
//!    conversion is needed — but the *Python* bindings of the same library report **character**
//!    offsets, because that is how Python indexes strings. The reference implementation therefore
//!    converts and this one must not: "Zażółć Anna" puts `Anna` at chars 7..11 and bytes 11..15,
//!    and applying the Python-side reasoning here produces empty or misplaced spans.
//! 3. **Every input the graph declares must be supplied.** HerBERT's IR takes `token_type_ids`
//!    that its tokenizer never emits; omitting them worked on FP32 and failed on INT8 with an
//!    opaque shape error.

use std::collections::HashMap;
use std::path::Path;

use openvino::{Core, DeviceType, Tensor};
use sentin_core::{DataKind, Finding, Layer, Validation};
use tokenizers::Tokenizer;

use crate::ov;

/// Why layer 2 could not be loaded, or could not run.
///
/// Every variant is survivable: the gateway logs it and keeps layer 1 running rather than refusing
/// traffic over a missing optional component.
#[derive(Debug, thiserror::Error)]
pub enum NerError {
    /// The tokenizer file is missing or unreadable. Usually a bundle without `tokenizer.json` —
    /// the diagnostic only compiles the graph, so it does not catch this.
    #[error("loading tokenizer from {path}: {source}")]
    Tokenizer {
        /// The path that was tried.
        path: String,
        /// What the `tokenizers` crate reported.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// A model file could not be read.
    #[error("reading {path}: {source}")]
    Io {
        /// The path that was tried.
        path: String,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// `config.json` carries no `id2label`, so predicted class indices cannot be named and BIO
    /// decoding has nothing to decode into.
    #[error("model config at {path} has no usable id2label map")]
    Labels {
        /// The config that was read.
        path: String,
    },
    /// The OpenVINO runtime refused: no loadable library, an unsupported graph, or a failed
    /// inference. Carries the message verbatim, since it is the only diagnostic the C API gives.
    #[error("OpenVINO: {0}")]
    OpenVino(String),
}

/// A loaded IR plus everything needed to turn text into findings.
pub struct NerEngine {
    tokenizer: Tokenizer,
    request: openvino::InferRequest,
    input_names: Vec<String>,
    id2label: Vec<String>,
    sequence_length: usize,
    device: String,
    fell_back: bool,
}

impl std::fmt::Debug for NerEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NerEngine")
            .field("device", &self.device)
            .field("fell_back", &self.fell_back)
            .field("sequence_length", &self.sequence_length)
            .field("labels", &self.id2label.len())
            .finish()
    }
}

impl NerEngine {
    /// Load an IR directory produced by `tools/prepare_model.py` / `quantize.py`.
    ///
    /// `device_request` is `NPU`, `GPU`, `CPU` or `AUTO`; the device that actually ran is recorded
    /// in [`NerEngine::device`] because "which device executed" is a fact this project logs.
    ///
    /// **A device that enumerates is not a device that works.** If the chosen one refuses to
    /// compile the model, the remaining devices are tried in [`ov::AUTO_ORDER`] before giving up,
    /// and the move is reported through [`NerEngine::fell_back`]. Resolving by enumeration alone
    /// would mean one unhappy NPU costs the gateway layer 2 entirely, on a machine with a working
    /// iGPU and CPU sitting right there — which is precisely what the invariant "NPU-first,
    /// CPU-fallback, transparent" exists to prevent.
    ///
    /// # Errors
    /// Fails if the tokenizer, config or IR cannot be loaded, or if no device accepts the model —
    /// in which case the error names every device tried and what each said.
    pub fn load(model_dir: &Path, device_request: &str) -> Result<Self, NerError> {
        let tokenizer_path = model_dir.join("tokenizer.json");
        let tokenizer =
            Tokenizer::from_file(&tokenizer_path).map_err(|source| NerError::Tokenizer {
                path: tokenizer_path.display().to_string(),
                source,
            })?;

        let config_path = model_dir.join("config.json");
        let id2label = read_labels(&config_path)?;

        let mut core = Core::new().map_err(|err| NerError::OpenVino(err.to_string()))?;
        let available: Vec<String> = core
            .available_devices()
            .map_err(|err| NerError::OpenVino(err.to_string()))?
            .iter()
            .map(ToString::to_string)
            .collect();
        let (candidates, resolved_elsewhere) = ov::device_candidates(device_request, &available);
        let first_choice = candidates[0].clone();

        let xml = model_dir.join("openvino_model.xml");
        let bin = model_dir.join("openvino_model.bin");
        let model = core
            .read_model_from_file(&xml.to_string_lossy(), &bin.to_string_lossy())
            .map_err(|err| NerError::OpenVino(format!("read_model: {err}")))?;

        let mut refusals = Vec::new();
        let mut chosen = None;
        for candidate in &candidates {
            match core.compile_model(&model, DeviceType::from(candidate.as_str())) {
                Ok(compiled) => {
                    chosen = Some((candidate.clone(), compiled));
                    break;
                }
                Err(err) => refusals.push(format!("{candidate}: {err}")),
            }
        }
        let Some((device, mut compiled)) = chosen else {
            return Err(NerError::OpenVino(format!(
                "no device would compile the model — {}",
                refusals.join("; ")
            )));
        };
        let fell_back = resolved_elsewhere || device != first_choice;

        // Shapes come from the compiled model: the source IR reports them only partially, and
        // asking it for a concrete shape fails even when the IR is static.
        let input_count = compiled
            .get_input_size()
            .map_err(|err| NerError::OpenVino(format!("get_input_size: {err}")))?;
        let mut input_names = Vec::with_capacity(input_count);
        let mut sequence_length = 0usize;
        for index in 0..input_count {
            let node = compiled
                .get_input_by_index(index)
                .map_err(|err| NerError::OpenVino(format!("get_input_by_index: {err}")))?;
            let name = node
                .get_name()
                .map_err(|err| NerError::OpenVino(format!("get_name: {err}")))?;
            let shape = node
                .get_shape()
                .map_err(|err| NerError::OpenVino(format!("get_shape: {err}")))?;
            // [batch, sequence]; the static sequence length is what everything must pad to.
            sequence_length = usize::try_from(shape.get_dimensions()[1].max(0)).unwrap_or(0);
            input_names.push(name);
        }

        let request = compiled
            .create_infer_request()
            .map_err(|err| NerError::OpenVino(format!("create_infer_request: {err}")))?;

        Ok(Self {
            tokenizer,
            request,
            input_names,
            id2label,
            sequence_length,
            device,
            fell_back,
        })
    }

    /// The device that actually executed, which is not necessarily the one requested.
    #[must_use]
    pub fn device(&self) -> &str {
        &self.device
    }

    /// True when the requested device was unavailable and another was used instead.
    #[must_use]
    pub fn fell_back(&self) -> bool {
        self.fell_back
    }

    /// The static sequence length this IR was reshaped to. Longer input is truncated to it;
    /// static shapes are an NPU requirement rather than a tuning choice.
    #[must_use]
    pub fn sequence_length(&self) -> usize {
        self.sequence_length
    }

    /// Find named entities in `text`, returning findings with **byte** spans.
    ///
    /// # Errors
    /// Fails if tokenization or inference fails.
    pub fn detect(&mut self, text: &str) -> Result<Vec<Finding>, NerError> {
        let encoding = self
            .tokenizer
            .encode(text, true)
            .map_err(|source| NerError::Tokenizer {
                path: "<encode>".to_string(),
                source,
            })?;

        let ids = encoding.get_ids();
        let take = ids.len().min(self.sequence_length);
        let mask = encoding.get_attention_mask();

        // Clone the names first: filling the tensors needs &mut self, so holding a borrow on
        // self.input_names across the loop would not compile.
        let names = self.input_names.clone();
        for name in &names {
            let mut buffer = vec![0i64; self.sequence_length];
            match name.as_str() {
                "input_ids" => {
                    for (slot, id) in buffer.iter_mut().zip(&ids[..take]) {
                        *slot = i64::from(*id);
                    }
                }
                "attention_mask" => {
                    for (slot, value) in buffer.iter_mut().zip(&mask[..take]) {
                        *slot = i64::from(*value);
                    }
                }
                // token_type_ids and anything else the graph declares: zeros are correct for a
                // single segment, and supplying them explicitly is not optional — see the module
                // docs. Leaving a declared input unset fails deep inside the graph.
                _ => {}
            }
            self.set_input(name, &buffer)?;
        }

        self.request
            .infer()
            .map_err(|err| NerError::OpenVino(format!("infer: {err}")))?;

        let output = self
            .request
            .get_output_tensor()
            .map_err(|err| NerError::OpenVino(format!("get_output_tensor: {err}")))?;
        let logits = output
            .get_data::<f32>()
            .map_err(|err| NerError::OpenVino(format!("output data: {err}")))?;

        let labels = self.id2label.len();
        let word_ids = encoding.get_word_ids();
        let offsets = encoding.get_offsets();

        Ok(decode(
            &self.id2label,
            logits,
            labels,
            &word_ids[..take],
            &offsets[..take],
            text,
        ))
    }

    fn set_input(&mut self, name: &str, data: &[i64]) -> Result<(), NerError> {
        let mut tensor = Tensor::new(
            openvino::ElementType::I64,
            &openvino::Shape::new(&[1, i64::try_from(data.len()).unwrap_or(0)])
                .map_err(|err| NerError::OpenVino(format!("shape: {err}")))?,
        )
        .map_err(|err| NerError::OpenVino(format!("tensor alloc: {err}")))?;
        tensor
            .get_data_mut::<i64>()
            .map_err(|err| NerError::OpenVino(format!("tensor data: {err}")))?
            .copy_from_slice(data);
        self.request
            .set_tensor(name, &tensor)
            .map_err(|err| NerError::OpenVino(format!("set_tensor({name}): {err}")))
    }
}

/// Aggregate per-token predictions into byte-span findings.
///
/// Separated from inference so the logic that actually decides span boundaries is unit-testable
/// without a model — that is where the mistakes live.
fn decode(
    id2label: &[String],
    logits: &[f32],
    label_count: usize,
    word_ids: &[Option<u32>],
    offsets: &[(usize, usize)],
    text: &str,
) -> Vec<Finding> {
    // Per word: the label of its first subword, and a character extent covering every subword.
    let mut order: Vec<u32> = Vec::new();
    let mut extents: HashMap<u32, (usize, usize)> = HashMap::new();
    let mut labels: HashMap<u32, &str> = HashMap::new();

    for (position, word) in word_ids.iter().enumerate() {
        let Some(word) = *word else { continue };
        let (start, end) = offsets[position];
        if end <= start {
            continue; // special tokens carry an empty span
        }

        let slice = &logits[position * label_count..(position + 1) * label_count];
        let best = slice
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map_or(0, |(index, _)| index);

        extents
            .entry(word)
            .and_modify(|(_, e)| *e = (*e).max(end))
            .or_insert_with(|| {
                order.push(word);
                labels.insert(word, id2label.get(best).map_or("O", String::as_str));
                (start, end)
            });
    }

    let mut findings: Vec<Finding> = Vec::new();
    let mut current: Option<(usize, usize, DataKind)> = None;

    for word in order {
        let (start, end) = extents[&word];
        let tag = labels[&word];
        let (prefix, entity) = tag.split_once('-').unwrap_or(("O", ""));
        let kind = entity_kind(entity);

        match kind {
            None => {
                if let Some(span) = current.take() {
                    findings.push(make_finding(span, text));
                }
            }
            Some(kind) => match &mut current {
                Some((_, current_end, current_kind)) if prefix == "I" && *current_kind == kind => {
                    *current_end = end;
                }
                slot => {
                    if let Some(span) = slot.take() {
                        findings.push(make_finding(span, text));
                    }
                    *slot = Some((start, end, kind));
                }
            },
        }
    }
    if let Some(span) = current {
        findings.push(make_finding(span, text));
    }
    findings
}

fn make_finding((start, end, kind): (usize, usize, DataKind), text: &str) -> Finding {
    debug_assert!(
        text.is_char_boundary(start) && text.is_char_boundary(end),
        "tokenizer offsets must already be byte offsets on a character boundary"
    );
    Finding {
        span: start..end,
        kind,
        // The model's own probability would be a better number, but a per-entity score needs the
        // softmax over the merged span; until that exists, do not invent precision.
        confidence: 0.9,
        layer: Layer::Ner,
        validation: Validation::Pattern,
    }
}

fn entity_kind(entity: &str) -> Option<DataKind> {
    match entity {
        "PER" => Some(DataKind::Person),
        "ORG" => Some(DataKind::Organization),
        "LOC" => Some(DataKind::Location),
        // DATE and anything else a model may predict is outside the shared label space.
        _ => None,
    }
}

fn read_labels(config_path: &Path) -> Result<Vec<String>, NerError> {
    let text = std::fs::read_to_string(config_path).map_err(|source| NerError::Io {
        path: config_path.display().to_string(),
        source,
    })?;
    let config: serde_json::Value = serde_json::from_str(&text).map_err(|_| NerError::Labels {
        path: config_path.display().to_string(),
    })?;
    let map = config
        .get("id2label")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| NerError::Labels {
            path: config_path.display().to_string(),
        })?;

    let mut labels = vec![String::from("O"); map.len()];
    for (key, value) in map {
        let index: usize = key.parse().map_err(|_| NerError::Labels {
            path: config_path.display().to_string(),
        })?;
        if let (Some(slot), Some(name)) = (labels.get_mut(index), value.as_str()) {
            *slot = name.to_string();
        }
    }
    Ok(labels)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_are_used_as_byte_offsets_not_character_offsets() {
        // Verified against the real tokenizer: "Zażółć Anna" puts Anna at bytes 11..15, while the
        // character positions are 7..11. Treating the crate's offsets as characters — which is
        // what the Python reference does with its own bindings — yields an empty or wrong span.
        let text = "Zażółć Anna";
        assert_eq!(text.chars().count(), 11);
        assert_eq!(text.len(), 15);

        let id2label = labels();
        let word_ids = [Some(0), Some(1)];
        let offsets = [(0, 10), (11, 15)];
        let logits = logits_for(&[0, 1], id2label.len());

        let findings = decode(
            &id2label,
            &logits,
            id2label.len(),
            &word_ids,
            &offsets,
            text,
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(&text[findings[0].span.clone()], "Anna");
    }

    /// Build logits that make `argmax` pick the requested label for each position.
    fn logits_for(picks: &[usize], label_count: usize) -> Vec<f32> {
        let mut out = vec![0.0; picks.len() * label_count];
        for (position, pick) in picks.iter().enumerate() {
            out[position * label_count + pick] = 10.0;
        }
        out
    }

    fn labels() -> Vec<String> {
        ["O", "B-PER", "I-PER", "B-ORG", "I-ORG", "B-LOC", "I-LOC"]
            .iter()
            .map(|s| (*s).to_string())
            .collect()
    }

    #[test]
    fn a_multi_word_entity_merges_into_one_span() {
        let text = "Pan Marek Nowak przyszedl";
        let id2label = labels();
        // words: Pan(O) Marek(B-PER) Nowak(I-PER) przyszedl(O)
        let word_ids = [Some(0), Some(1), Some(2), Some(3)];
        let offsets = [(0, 3), (4, 9), (10, 15), (16, 25)];
        let logits = logits_for(&[0, 1, 2, 0], id2label.len());

        let findings = decode(
            &id2label,
            &logits,
            id2label.len(),
            &word_ids,
            &offsets,
            text,
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(&text[findings[0].span.clone()], "Marek Nowak");
        assert_eq!(findings[0].kind, DataKind::Person);
    }

    #[test]
    fn a_word_split_into_subwords_keeps_its_full_extent() {
        // This is the bug that cost 63 F1 points in the Python reference: the label comes from the
        // first subword, but the extent has to cover all of them.
        let text = "Marek Wisniowiecki";
        let id2label = labels();
        // "Wisniowiecki" arrives as three subwords sharing word id 1.
        let word_ids = [Some(0), Some(1), Some(1), Some(1)];
        let offsets = [(0, 5), (6, 11), (11, 15), (15, 18)];
        let logits = logits_for(&[1, 2, 0, 0], id2label.len());

        let findings = decode(
            &id2label,
            &logits,
            id2label.len(),
            &word_ids,
            &offsets,
            text,
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(&text[findings[0].span.clone()], "Marek Wisniowiecki");
    }

    #[test]
    fn two_adjacent_entities_of_the_same_type_stay_separate() {
        // B- always starts a new entity, even directly after another of the same type.
        let text = "Anna Ewa";
        let id2label = labels();
        let word_ids = [Some(0), Some(1)];
        let offsets = [(0, 4), (5, 8)];
        let logits = logits_for(&[1, 1], id2label.len());

        let findings = decode(
            &id2label,
            &logits,
            id2label.len(),
            &word_ids,
            &offsets,
            text,
        );
        assert_eq!(
            findings.len(),
            2,
            "B- must not continue the previous entity"
        );
    }

    #[test]
    fn labels_outside_the_shared_space_are_ignored() {
        let text = "Spotkanie wczoraj";
        let mut id2label = labels();
        id2label.push("B-DATE".to_string());
        let date = id2label.len() - 1;
        let word_ids = [Some(0), Some(1)];
        let offsets = [(0, 9), (10, 17)];
        let logits = logits_for(&[0, date], id2label.len());

        let findings = decode(
            &id2label,
            &logits,
            id2label.len(),
            &word_ids,
            &offsets,
            text,
        );
        assert!(findings.is_empty(), "DATE is not in PER/ORG/LOC");
    }

    #[test]
    fn ner_findings_can_never_block() {
        let text = "Jan Kowalski";
        let id2label = labels();
        let word_ids = [Some(0), Some(1)];
        let offsets = [(0, 3), (4, 12)];
        let logits = logits_for(&[1, 2], id2label.len());

        let findings = decode(
            &id2label,
            &logits,
            id2label.len(),
            &word_ids,
            &offsets,
            text,
        );
        assert_eq!(findings[0].layer, Layer::Ner);
        assert_eq!(findings[0].validation, Validation::Pattern);
        assert_eq!(
            findings[0].clamp_decision(sentin_core::Decision::Blocked),
            sentin_core::Decision::Masked
        );
    }
}
