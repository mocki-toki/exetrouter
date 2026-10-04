# Model export examples

`exr models --json --format FORMAT` prints JSON to stdout. It reads the account-pool extractor; no inference or API-token creation is involved. Without `--format`, JSON uses `openai-json`.

These complete outputs use **one synthetic catalog entry** with ID `gpt-5.6-sol`. Limits and capabilities below are illustrative, not claims about that model's current upstream settings.

The reasoning levels match the account-visible catalog observed on 2026-10-04: `low`, `medium`, `high`, `xhigh`, `max`, `ultra`. Each real export uses the levels reported by its own pool. V1 marks only absent built-in presets (`none` and `minimal` in this example) as disabled, so OpenCode does not add them automatically; this does not reject API requests at the router.

Real exports use your pool's reported metadata and include every visible model. Neither the API bearer nor SSH key material appears in the output.

| Format | Root and purpose |
| --- | --- |
| `openai-json` | OpenAI-style `object`/`data` list, with reported fields in each model's `exetrouter` metadata. |
| `codex-json` | A `models` array of Codex ModelInfo descriptors; no provider connection settings. |
| `opencode-v1-json` | Singular `provider`, V1 options and variant objects. |
| `opencode-v2-json` | Plural `providers`, V2 settings/capabilities and variant arrays. |

For the OpenCode examples the remote HTTP URL has already been saved once with `exr configure --api-url https://api.example.com/v1`. Standalone instead derives its local API address automatically. `--base-url` only overrides one export. The optional `--model exetrouter/gpt-5.6-sol` selects an imported catalog model in the configuration; it does not reduce the model list. Without that flag, neither OpenCode format emits a top-level `model`, leaving selection to OpenCode's configured/recent/default preferences, which may choose another provider.

## openai-json

```sh
exr models --json --format openai-json
```

```json
{
  "data": [
    {
      "display_name": "GPT-5.6 Sol",
      "exetrouter": {
        "auto_compact_token_limit": 80000,
        "context_window": 100000,
        "default_reasoning_level": "low",
        "default_reasoning_summary": "none",
        "default_verbosity": "low",
        "description": "Illustrative model metadata",
        "display_name": "GPT-5.6 Sol",
        "effective_context_window_percent": 95,
        "experimental_supported_tools": [],
        "input_modalities": [
          "text",
          "image"
        ],
        "priority": 1,
        "shell_type": "unified_exec",
        "slug": "gpt-5.6-sol",
        "support_verbosity": true,
        "supported_in_api": true,
        "supported_reasoning_levels": [
          {
            "description": "Low",
            "effort": "low"
          },
          {
            "description": "Medium",
            "effort": "medium"
          },
          {
            "description": "High",
            "effort": "high"
          },
          {
            "description": "Xhigh",
            "effort": "xhigh"
          },
          {
            "description": "Max",
            "effort": "max"
          },
          {
            "description": "Ultra",
            "effort": "ultra"
          }
        ],
        "truncation_policy": {
          "limit": 10000,
          "mode": "tokens"
        },
        "visibility": "list"
      },
      "id": "gpt-5.6-sol",
      "object": "model",
      "owned_by": "openai"
    }
  ],
  "object": "list"
}
```

## codex-json

```sh
exr models --json --format codex-json
```

```json
{
  "models": [
    {
      "auto_compact_token_limit": 80000,
      "context_window": 100000,
      "default_reasoning_level": "low",
      "default_reasoning_summary": "none",
      "default_verbosity": "low",
      "description": "Illustrative model metadata",
      "display_name": "GPT-5.6 Sol",
      "effective_context_window_percent": 95,
      "experimental_supported_tools": [],
      "input_modalities": [
        "text",
        "image"
      ],
      "priority": 1,
      "shell_type": "unified_exec",
      "slug": "gpt-5.6-sol",
      "support_verbosity": true,
      "supported_in_api": true,
      "supported_reasoning_levels": [
        {
          "description": "Low",
          "effort": "low"
        },
        {
          "description": "Medium",
          "effort": "medium"
        },
        {
          "description": "High",
          "effort": "high"
        },
        {
          "description": "Xhigh",
          "effort": "xhigh"
        },
        {
          "description": "Max",
          "effort": "max"
        },
        {
          "description": "Ultra",
          "effort": "ultra"
        }
      ],
      "truncation_policy": {
        "limit": 10000,
        "mode": "tokens"
      },
      "visibility": "list"
    }
  ]
}
```

## opencode-v1-json

```sh
exr models --json --format opencode-v1-json --model exetrouter/gpt-5.6-sol
```

```json
{
  "model": "exetrouter/gpt-5.6-sol",
  "provider": {
    "exetrouter": {
      "env": [
        "EXETROUTER_TOKEN"
      ],
      "models": {
        "gpt-5.6-sol": {
          "limit": {
            "context": 100000,
            "input": 80000,
            "output": 0
          },
          "modalities": {
            "input": [
              "text",
              "image"
            ],
            "output": [
              "text"
            ]
          },
          "name": "GPT-5.6 Sol",
          "options": {
            "reasoningEffort": "low",
            "reasoningSummary": "none",
            "store": false,
            "textVerbosity": "low"
          },
          "reasoning": true,
          "tool_call": true,
          "variants": {
            "high": {
              "reasoningEffort": "high"
            },
            "low": {
              "reasoningEffort": "low"
            },
            "max": {
              "reasoningEffort": "max"
            },
            "medium": {
              "reasoningEffort": "medium"
            },
            "minimal": {
              "disabled": true
            },
            "none": {
              "disabled": true
            },
            "ultra": {
              "reasoningEffort": "ultra"
            },
            "xhigh": {
              "reasoningEffort": "xhigh"
            }
          }
        }
      },
      "name": "ExetRouter",
      "npm": "@ai-sdk/openai",
      "options": {
        "apiKey": "{env:EXETROUTER_TOKEN}",
        "baseURL": "https://api.example.com/v1"
      }
    }
  }
}
```

## opencode-v2-json

```sh
exr models --json --format opencode-v2-json --model exetrouter/gpt-5.6-sol
```

```json
{
  "model": {
    "model": "gpt-5.6-sol",
    "providerID": "exetrouter"
  },
  "providers": {
    "exetrouter": {
      "env": [
        "EXETROUTER_TOKEN"
      ],
      "models": {
        "gpt-5.6-sol": {
          "capabilities": {
            "input": [
              "text",
              "image"
            ],
            "output": [
              "text"
            ],
            "tools": true
          },
          "limit": {
            "context": 100000,
            "input": 80000,
            "output": 0
          },
          "name": "GPT-5.6 Sol",
          "settings": {
            "reasoningEffort": "low",
            "reasoningSummary": "none",
            "textVerbosity": "low"
          },
          "variants": [
            {
              "id": "low",
              "settings": {
                "reasoningEffort": "low"
              }
            },
            {
              "id": "medium",
              "settings": {
                "reasoningEffort": "medium"
              }
            },
            {
              "id": "high",
              "settings": {
                "reasoningEffort": "high"
              }
            },
            {
              "id": "xhigh",
              "settings": {
                "reasoningEffort": "xhigh"
              }
            },
            {
              "id": "max",
              "settings": {
                "reasoningEffort": "max"
              }
            },
            {
              "id": "ultra",
              "settings": {
                "reasoningEffort": "ultra"
              }
            }
          ]
        }
      },
      "name": "ExetRouter",
      "package": "@opencode/ai/providers/openai/responses",
      "settings": {
        "baseURL": "https://api.example.com/v1",
        "compaction": {
          "type": "native"
        },
        "store": false,
        "transport": "websocket"
      }
    }
  }
}
```
