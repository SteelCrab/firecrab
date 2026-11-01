# Official application icons

These assets identify the providers in AI Sandbox and Pipe AI. Their colors and artwork are not redrawn or recolored.

- `chatgpt.png`: the original 128×128 PNG payload (`ic07`) in the current official OpenAI app's primary `electron.icns` (bundle `com.openai.codex`, version 26.1027.30824). `CFBundleIconFile` selects `electron.icns`. This is also identical to the PNG payload in `icon-chatgpt.icns`. It is extracted byte-for-byte without resampling or recoloring. Both ChatGPT sign-in and the Codex CLI use this current app identity in Firecrab.
  - Product: https://openai.com/codex/
  - SHA-256: `b0e1a6a8a0793be1de7c1714bad83c8d9d0525d9676edafa05d7d5f85f933db2`
- `claude.png`: the original 128×128 PNG payload (`ic07`) in the official Claude app bundle (`com.anthropic.claudefordesktop`, version 4.4212.0). Extracted byte-for-byte from `Claude.app/Contents/Resources/electron.icns`. It is not resampled or re-encoded.
  - Product: https://claude.com/product/claude-code
  - SHA-256: `1f249bbb778ae928247321ef0f4b40cb4edeb8fd896e344f4c978df48f724be6`

The OpenAI/ChatGPT artwork belongs to OpenAI. The Claude artwork belongs to Anthropic. These brand assets are not covered by the LobeHub Icons MIT license. They identify the corresponding provider and do not imply endorsement of Firecrab.

GitHub mark: official Primer Octicons `mark-github-16.svg`, https://github.com/primer/octicons/blob/main/icons/mark-github-16.svg (MIT).
