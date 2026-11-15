# Native Gemini API adapter

Ryoko supports the native Google Gemini \`generateContent\` API in
\`ryoko-models\`, alongside OpenAI-compatible and Anthropic APIs. It
does not require Gemini to expose the OpenAI compatibility surface.

Provider catalog example:

\`\`\`json
{
  "default_provider": "gemini",
  "providers": [
    {
      "id": "gemini",
      "base_url": "https://generativelanguage.googleapis.com/v1beta",
      "api_key_env": "GEMINI_API_KEY",
      "wire": "gemini"
    }
  ]
}
\`\`\`

Set \`GEMINI_API_KEY\` in the operator environment. Only the variable name
belongs in JSON. The key is transmitted in an \`x-goog-api-key\` header,
not in a URL, and provider error bodies are suppressed in diagnostics.

Native support includes system instructions, multi-turn text, function
declarations, tool call/result roundtrips, token usage accounting and
\`maxOutputTokens\` with a finite default. The adapter currently sends one
bounded non-streaming \`generateContent\` call and converts its reply into
Ryoko's normalized event stream. Server-Sent Event incremental streaming,
image/audio payloads, native Google IAM, Vertex AI and AWS Bedrock are not
yet implemented. Provider-specific prices must be supplied by a trusted
operator; do not interpret a missing price as a verified zero-dollar cost.

**Validation boundaries:** Gemini requires HTTPS and an explicit
\`api_key_env\` profile field, and rejects chat-path overrides. Model names
are checked before joining the endpoint to prevent path manipulation.
Account/region quotas and paid usage still apply. Ryoko must not
automatically select a paid model without configured spending controls.
