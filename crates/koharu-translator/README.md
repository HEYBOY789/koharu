# Koharu Translator

koharu-translator provides one segment-preserving translation interface for
local GGUF models and hosted providers.

Model metadata is provider-neutral: each model has a stable provider ID, a
display name, and (for local models) selectable GGUF quantizations. Every
provider module owns its connection config, defaults, request mapping, and
model catalog or discovery.

The crate owns only provider connectivity. Provider settings such as base URLs
are persisted under `[providers]`; credentials remain in the platform keychain.
Translation model selection, target language, instructions, and generation
options belong to the caller. `koharu-pipeline` owns those values as the
configuration of its translation processor.

`Translator` is an execution engine. A caller supplies a `ModelSelection`,
`GenerationConfig`, and `TranslationRequest` for each operation. The engine
keeps the selected local model resident when possible and reads live provider
connection settings without owning workflow configuration.

For Gemini model IDs on an OpenAI-compatible endpoint (including provider-prefixed
IDs such as `ag/gemini-3.8-flash-high`), the reasoning toggle sends `low` when off
and `high` when on. Gemini 3.8 requires thinking, so off reduces it rather than
disabling it. This also avoids `medium` on older Gemini Pro models that only accept
`low` and `high`. An unspecified setting omits `reasoning_effort`, leaving the
endpoint's model preset in control. Other model IDs keep the existing
`none`/`medium` mapping. The selected model ID itself is not rewritten.
Compatible requests explicitly set `stream: false` because the response reader
expects one JSON completion; this also prevents proxies from choosing an SSE
response when the field is omitted.

Invalid OpenAI-compatible completions (including malformed JSON and invalid IDs)
are discarded and use the pipeline's existing bounded, cancellable recovery
schedule. No content is repaired or committed from that response. If retries are
exhausted the page stays unfinished; the error reports the parse/validation reason
without flooding Activity with the model's repeating output. Account/authentication
failures remain terminal, and other providers' recovery policies are unchanged.
The compatible reader also rejects a likely generation loop when an output is
longer than both 1,024 characters and eight times its source, and contains at
least 64 identical consecutive whitespace-separated tokens. It retries the whole
completion rather than shortening the translation. A source that is itself long
and repetitive is not rejected by this expansion check.

The shared translation reader accepts bare JSON or a single complete `json`/unlabelled
code fence around it, including a compact opening such as ` ```json{ `. Only that
outer framing is removed. It still rejects malformed/truncated JSON, surrounding
explanations, multiple documents and missing/duplicate/out-of-range segment IDs;
translated strings are preserved exactly. Strict structured-output requests remain
enabled, and the prompt explicitly prohibits Markdown framing.

References: [Google thinking levels](https://ai.google.dev/gemini-api/docs/thinking)
and [Gemini 3 thinking controls](https://docs.cloud.google.com/gemini-enterprise-agent-platform/models/start/get-started-with-gemini-3).
