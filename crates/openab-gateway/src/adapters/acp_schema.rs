//! ACP v1 wire types — GENERATED, do not edit by hand.
//!
//! Source schema: `agentclientprotocol/agent-client-protocol`
//!   `schema/v1/schema.json` @ `eb88e992521c350fc87b51207baa4ea4877ce0dd` (ACP Schema v1.19.0),
//!   vendored at `crates/openab-gateway/schemas/acp-v1.schema.json`.
//! Generator: `cargo-typify 0.7.0` — plain serde, **no** `schemars` / `serde_with` runtime dep.
//! Regenerate:
//!   `cargo typify --output crates/openab-gateway/src/adapters/acp_schema.rs \`
//!     `crates/openab-gateway/schemas/acp-v1.schema.json`
//!
//! The full v1 surface is generated (it is one closed dependency graph — `SessionUpdate`
//! alone reaches most defs, so a hand-trimmed "chat subset" would not be meaningfully
//! smaller and would diverge from the schema). The base wires only the chat subset
//! (Initialize / NewSession / Resume / Prompt / StopReason / ContentBlock /
//! SessionNotification); the rest (tool_call, fs/*, terminal/*, permission) is inert
//! until the bidirectional / MCP-over-ACP roadmap consumes it.

#![allow(clippy::redundant_closure_call)]
#![allow(clippy::needless_lifetimes)]
#![allow(clippy::match_single_binding)]
#![allow(clippy::clone_on_copy)]

#[doc = r" Error types."]
pub mod error {
    #[doc = r" Error from a `TryFrom` or `FromStr` implementation."]
    pub struct ConversionError(::std::borrow::Cow<'static, str>);
    impl ::std::error::Error for ConversionError {}
    impl ::std::fmt::Display for ConversionError {
        fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> Result<(), ::std::fmt::Error> {
            ::std::fmt::Display::fmt(&self.0, f)
        }
    }
    impl ::std::fmt::Debug for ConversionError {
        fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> Result<(), ::std::fmt::Error> {
            ::std::fmt::Debug::fmt(&self.0, f)
        }
    }
    impl From<&'static str> for ConversionError {
        fn from(value: &'static str) -> Self {
            Self(value.into())
        }
    }
    impl From<String> for ConversionError {
        fn from(value: String) -> Self {
            Self(value.into())
        }
    }
}
#[doc = "A message (request, response, or notification) with `\"jsonrpc\": \"2.0\"` specified as\n[required by JSON-RPC 2.0 Specification][1].\n\n[1]: https://www.jsonrpc.org/specification#compatibility"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"Agent\","]
#[doc = "  \"description\": \"A message (request, response, or notification) with `\\\"jsonrpc\\\": \\\"2.0\\\"` specified as\\n[required by JSON-RPC 2.0 Specification][1].\\n\\n[1]: https://www.jsonrpc.org/specification#compatibility\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"anyOf\": ["]
#[doc = "    {"]
#[doc = "      \"title\": \"Request\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/AgentRequest\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"Response\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/AgentResponse\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"Notification\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/AgentNotification\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  ],"]
#[doc = "  \"required\": ["]
#[doc = "    \"jsonrpc\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"jsonrpc\": {"]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"enum\": ["]
#[doc = "        \"2.0\""]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
#[serde(untagged)]
pub enum Agent {
    Variant0 {
        #[doc = "The request id used to correlate the matching response."]
        id: RequestId,
        jsonrpc: AgentVariant0Jsonrpc,
        #[doc = "The method name to invoke."]
        method: ::std::string::String,
        #[doc = "Method-specific request parameters."]
        #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
        params: ::std::option::Option<AgentVariant0Params>,
    },
    Variant1(AgentVariant1),
    Variant2 {
        jsonrpc: AgentVariant2Jsonrpc,
        #[doc = "The notification method name."]
        method: ::std::string::String,
        #[doc = "Method-specific notification parameters."]
        #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
        params: ::std::option::Option<AgentVariant2Params>,
    },
}
impl ::std::convert::From<AgentVariant1> for Agent {
    fn from(value: AgentVariant1) -> Self {
        Self::Variant1(value)
    }
}
#[doc = "Authentication-related capabilities supported by the agent."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Authentication-related capabilities supported by the agent.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"logout\": {"]
#[doc = "      \"description\": \"Whether the agent supports the logout method.\\n\\nOptional. Omitted or `null` both mean the agent does not advertise support.\\nSupplying `{}` means the agent supports the logout method.\","]
#[doc = "      \"anyOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/LogoutCapabilities\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"type\": \"null\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct AgentAuthCapabilities {
    #[doc = "Whether the agent supports the logout method.\n\nOptional. Omitted or `null` both mean the agent does not advertise support.\nSupplying `{}` means the agent supports the logout method."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub logout: ::std::option::Option<LogoutCapabilities>,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
}
impl ::std::default::Default for AgentAuthCapabilities {
    fn default() -> Self {
        Self {
            logout: Default::default(),
            meta: Default::default(),
        }
    }
}
impl AgentAuthCapabilities {
    pub fn builder() -> builder::AgentAuthCapabilities {
        Default::default()
    }
}
#[doc = "Capabilities supported by the agent.\n\nAdvertised during initialization to inform the client about\navailable features and content types.\n\nSee protocol docs: [Agent Capabilities](https://agentclientprotocol.com/protocol/initialization#agent-capabilities)"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Capabilities supported by the agent.\\n\\nAdvertised during initialization to inform the client about\\navailable features and content types.\\n\\nSee protocol docs: [Agent Capabilities](https://agentclientprotocol.com/protocol/initialization#agent-capabilities)\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"auth\": {"]
#[doc = "      \"description\": \"Authentication-related capabilities supported by the agent.\","]
#[doc = "      \"default\": {},"]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/AgentAuthCapabilities\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"loadSession\": {"]
#[doc = "      \"description\": \"Whether the agent supports `session/load`.\","]
#[doc = "      \"default\": false,"]
#[doc = "      \"type\": \"boolean\","]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"mcpCapabilities\": {"]
#[doc = "      \"description\": \"MCP capabilities supported by the agent.\","]
#[doc = "      \"default\": {"]
#[doc = "        \"http\": false,"]
#[doc = "        \"sse\": false"]
#[doc = "      },"]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/McpCapabilities\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"promptCapabilities\": {"]
#[doc = "      \"description\": \"Prompt capabilities supported by the agent.\","]
#[doc = "      \"default\": {"]
#[doc = "        \"audio\": false,"]
#[doc = "        \"embeddedContext\": false,"]
#[doc = "        \"image\": false"]
#[doc = "      },"]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/PromptCapabilities\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"sessionCapabilities\": {"]
#[doc = "      \"description\": \"Session lifecycle and prompt capabilities advertised by the agent.\","]
#[doc = "      \"default\": {},"]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/SessionCapabilities\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct AgentCapabilities {
    #[doc = "Authentication-related capabilities supported by the agent."]
    #[serde(default = "defaults::agent_capabilities_auth")]
    pub auth: AgentAuthCapabilities,
    #[doc = "Whether the agent supports `session/load`."]
    #[serde(rename = "loadSession", default)]
    pub load_session: bool,
    #[doc = "MCP capabilities supported by the agent."]
    #[serde(
        rename = "mcpCapabilities",
        default = "defaults::agent_capabilities_mcp_capabilities"
    )]
    pub mcp_capabilities: McpCapabilities,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "Prompt capabilities supported by the agent."]
    #[serde(
        rename = "promptCapabilities",
        default = "defaults::agent_capabilities_prompt_capabilities"
    )]
    pub prompt_capabilities: PromptCapabilities,
    #[doc = "Session lifecycle and prompt capabilities advertised by the agent."]
    #[serde(
        rename = "sessionCapabilities",
        default = "defaults::agent_capabilities_session_capabilities"
    )]
    pub session_capabilities: SessionCapabilities,
}
impl ::std::default::Default for AgentCapabilities {
    fn default() -> Self {
        Self {
            auth: defaults::agent_capabilities_auth(),
            load_session: Default::default(),
            mcp_capabilities: defaults::agent_capabilities_mcp_capabilities(),
            meta: Default::default(),
            prompt_capabilities: defaults::agent_capabilities_prompt_capabilities(),
            session_capabilities: defaults::agent_capabilities_session_capabilities(),
        }
    }
}
impl AgentCapabilities {
    pub fn builder() -> builder::AgentCapabilities {
        Default::default()
    }
}
#[doc = "`AgentClientProtocol`"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"Agent Client Protocol\","]
#[doc = "  \"anyOf\": ["]
#[doc = "    {"]
#[doc = "      \"title\": \"Agent\","]
#[doc = "      \"description\": \"A message (request, response, or notification) with `\\\"jsonrpc\\\": \\\"2.0\\\"` specified as\\n[required by JSON-RPC 2.0 Specification][1].\\n\\n[1]: https://www.jsonrpc.org/specification#compatibility\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"anyOf\": ["]
#[doc = "        {"]
#[doc = "          \"title\": \"Request\","]
#[doc = "          \"allOf\": ["]
#[doc = "            {"]
#[doc = "              \"$ref\": \"#/$defs/AgentRequest\""]
#[doc = "            }"]
#[doc = "          ]"]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"title\": \"Response\","]
#[doc = "          \"allOf\": ["]
#[doc = "            {"]
#[doc = "              \"$ref\": \"#/$defs/AgentResponse\""]
#[doc = "            }"]
#[doc = "          ]"]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"title\": \"Notification\","]
#[doc = "          \"allOf\": ["]
#[doc = "            {"]
#[doc = "              \"$ref\": \"#/$defs/AgentNotification\""]
#[doc = "            }"]
#[doc = "          ]"]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"required\": ["]
#[doc = "        \"jsonrpc\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"jsonrpc\": {"]
#[doc = "          \"type\": \"string\","]
#[doc = "          \"enum\": ["]
#[doc = "            \"2.0\""]
#[doc = "          ]"]
#[doc = "        }"]
#[doc = "      }"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"Client\","]
#[doc = "      \"description\": \"A message (request, response, or notification) with `\\\"jsonrpc\\\": \\\"2.0\\\"` specified as\\n[required by JSON-RPC 2.0 Specification][1].\\n\\n[1]: https://www.jsonrpc.org/specification#compatibility\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"anyOf\": ["]
#[doc = "        {"]
#[doc = "          \"title\": \"Request\","]
#[doc = "          \"allOf\": ["]
#[doc = "            {"]
#[doc = "              \"$ref\": \"#/$defs/ClientRequest\""]
#[doc = "            }"]
#[doc = "          ]"]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"title\": \"Response\","]
#[doc = "          \"allOf\": ["]
#[doc = "            {"]
#[doc = "              \"$ref\": \"#/$defs/ClientResponse\""]
#[doc = "            }"]
#[doc = "          ]"]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"title\": \"Notification\","]
#[doc = "          \"allOf\": ["]
#[doc = "            {"]
#[doc = "              \"$ref\": \"#/$defs/ClientNotification\""]
#[doc = "            }"]
#[doc = "          ]"]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"required\": ["]
#[doc = "        \"jsonrpc\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"jsonrpc\": {"]
#[doc = "          \"type\": \"string\","]
#[doc = "          \"enum\": ["]
#[doc = "            \"2.0\""]
#[doc = "          ]"]
#[doc = "        }"]
#[doc = "      }"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"ProtocolLevel\","]
#[doc = "      \"description\": \"A message (request, response, or notification) with `\\\"jsonrpc\\\": \\\"2.0\\\"` specified as\\n[required by JSON-RPC 2.0 Specification][1].\\n\\n[1]: https://www.jsonrpc.org/specification#compatibility\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"required\": ["]
#[doc = "        \"jsonrpc\","]
#[doc = "        \"method\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"jsonrpc\": {"]
#[doc = "          \"type\": \"string\","]
#[doc = "          \"enum\": ["]
#[doc = "            \"2.0\""]
#[doc = "          ]"]
#[doc = "        },"]
#[doc = "        \"method\": {"]
#[doc = "          \"description\": \"The notification method name.\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        },"]
#[doc = "        \"params\": {"]
#[doc = "          \"description\": \"Method-specific notification parameters.\","]
#[doc = "          \"anyOf\": ["]
#[doc = "            {"]
#[doc = "              \"description\": \"General protocol-level notifications that all sides are expected to\\nimplement.\\n\\nNotifications whose methods start with '$/' are messages which\\nare protocol implementation dependent and might not be implementable in all\\nclients or agents. For example if the implementation uses a single threaded\\nsynchronous programming language then there is little it can do to react to\\na `$/cancel_request` notification. If an agent or client receives\\nnotifications starting with '$/' it is free to ignore the notification.\\n\\nNotifications do not expect a response.\","]
#[doc = "              \"anyOf\": ["]
#[doc = "                {"]
#[doc = "                  \"title\": \"CancelRequestNotification\","]
#[doc = "                  \"description\": \"Cancels an ongoing request.\\n\\nThis is a notification sent by the side that sent a request to cancel that request.\\n\\nUpon receiving this notification, the receiver:\\n\\n1. MAY cancel the corresponding request activity and all nested activities\\n2. MAY send any pending notifications.\\n3. MUST send one of these responses for the original request:\\n  - Valid response with appropriate data (partial results or cancellation marker)\\n  - Error response with code `-32800` (Cancelled)\\n\\nSee protocol docs: [Cancellation](https://agentclientprotocol.com/protocol/cancellation)\","]
#[doc = "                  \"allOf\": ["]
#[doc = "                    {"]
#[doc = "                      \"$ref\": \"#/$defs/CancelRequestNotification\""]
#[doc = "                    }"]
#[doc = "                  ]"]
#[doc = "                }"]
#[doc = "              ]"]
#[doc = "            },"]
#[doc = "            {"]
#[doc = "              \"type\": \"null\""]
#[doc = "            }"]
#[doc = "          ]"]
#[doc = "        }"]
#[doc = "      },"]
#[doc = "      \"x-docs-ignore\": true"]
#[doc = "    }"]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct AgentClientProtocol {
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_0: ::std::option::Option<Agent>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_1: ::std::option::Option<Client>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_2: ::std::option::Option<ProtocolLevel>,
}
impl ::std::default::Default for AgentClientProtocol {
    fn default() -> Self {
        Self {
            subtype_0: Default::default(),
            subtype_1: Default::default(),
            subtype_2: Default::default(),
        }
    }
}
impl AgentClientProtocol {
    pub fn builder() -> builder::AgentClientProtocol {
        Default::default()
    }
}
#[doc = "A JSON-RPC notification object."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"A JSON-RPC notification object.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"method\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"method\": {"]
#[doc = "      \"description\": \"The notification method name.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"params\": {"]
#[doc = "      \"description\": \"Method-specific notification parameters.\","]
#[doc = "      \"anyOf\": ["]
#[doc = "        {"]
#[doc = "          \"description\": \"All possible notifications that an agent can send to a client.\\n\\nThis enum is used internally for routing RPC notifications. You typically won't need\\nto use this directly.\\n\\nNotifications do not expect a response.\","]
#[doc = "          \"anyOf\": ["]
#[doc = "            {"]
#[doc = "              \"title\": \"SessionNotification\","]
#[doc = "              \"description\": \"Handles session update notifications from the agent.\\n\\nThis is a notification endpoint (no response expected) that receives\\nreal-time updates about session progress, including message chunks,\\ntool calls, and execution plans.\\n\\nNote: Clients SHOULD continue accepting tool call updates even after\\nsending a `session/cancel` notification, as the agent may send final\\nupdates before responding with the cancelled stop reason.\\n\\nSee protocol docs: [Agent Reports Output](https://agentclientprotocol.com/protocol/prompt-turn#3-agent-reports-output)\","]
#[doc = "              \"allOf\": ["]
#[doc = "                {"]
#[doc = "                  \"$ref\": \"#/$defs/SessionNotification\""]
#[doc = "                }"]
#[doc = "              ]"]
#[doc = "            },"]
#[doc = "            {"]
#[doc = "              \"title\": \"ExtNotification\","]
#[doc = "              \"description\": \"Handles extension notifications from the agent.\\n\\nAllows the Agent to send an arbitrary notification that is not part of the ACP spec.\\nExtension notifications provide a way to send one-way messages for custom functionality\\nwhile maintaining protocol compatibility.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "              \"allOf\": ["]
#[doc = "                {"]
#[doc = "                  \"$ref\": \"#/$defs/ExtNotification\""]
#[doc = "                }"]
#[doc = "              ]"]
#[doc = "            }"]
#[doc = "          ]"]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"type\": \"null\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"x-docs-ignore\": true"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct AgentNotification {
    #[doc = "The notification method name."]
    pub method: ::std::string::String,
    #[doc = "Method-specific notification parameters."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub params: ::std::option::Option<AgentNotificationParams>,
}
impl AgentNotification {
    pub fn builder() -> builder::AgentNotification {
        Default::default()
    }
}
#[doc = "All possible notifications that an agent can send to a client.\n\nThis enum is used internally for routing RPC notifications. You typically won't need\nto use this directly.\n\nNotifications do not expect a response."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"All possible notifications that an agent can send to a client.\\n\\nThis enum is used internally for routing RPC notifications. You typically won't need\\nto use this directly.\\n\\nNotifications do not expect a response.\","]
#[doc = "  \"anyOf\": ["]
#[doc = "    {"]
#[doc = "      \"title\": \"SessionNotification\","]
#[doc = "      \"description\": \"Handles session update notifications from the agent.\\n\\nThis is a notification endpoint (no response expected) that receives\\nreal-time updates about session progress, including message chunks,\\ntool calls, and execution plans.\\n\\nNote: Clients SHOULD continue accepting tool call updates even after\\nsending a `session/cancel` notification, as the agent may send final\\nupdates before responding with the cancelled stop reason.\\n\\nSee protocol docs: [Agent Reports Output](https://agentclientprotocol.com/protocol/prompt-turn#3-agent-reports-output)\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/SessionNotification\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"ExtNotification\","]
#[doc = "      \"description\": \"Handles extension notifications from the agent.\\n\\nAllows the Agent to send an arbitrary notification that is not part of the ACP spec.\\nExtension notifications provide a way to send one-way messages for custom functionality\\nwhile maintaining protocol compatibility.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/ExtNotification\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct AgentNotificationParams {
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_0: ::std::option::Option<SessionNotification>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_1: ::std::option::Option<ExtNotification>,
}
impl ::std::default::Default for AgentNotificationParams {
    fn default() -> Self {
        Self {
            subtype_0: Default::default(),
            subtype_1: Default::default(),
        }
    }
}
impl AgentNotificationParams {
    pub fn builder() -> builder::AgentNotificationParams {
        Default::default()
    }
}
#[doc = "A JSON-RPC request object."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"A JSON-RPC request object.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"id\","]
#[doc = "    \"method\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"id\": {"]
#[doc = "      \"description\": \"The request id used to correlate the matching response.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/RequestId\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    \"method\": {"]
#[doc = "      \"description\": \"The method name to invoke.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"params\": {"]
#[doc = "      \"description\": \"Method-specific request parameters.\","]
#[doc = "      \"anyOf\": ["]
#[doc = "        {"]
#[doc = "          \"description\": \"All possible requests that an agent can send to a client.\\n\\nThis enum is used internally for routing RPC requests. You typically won't need\\nto use this directly.\\n\\nThis enum encompasses all method calls from agent to client.\","]
#[doc = "          \"anyOf\": ["]
#[doc = "            {"]
#[doc = "              \"title\": \"WriteTextFileRequest\","]
#[doc = "              \"description\": \"Writes content to a text file in the client's file system.\\n\\nOnly available if the client advertises the `fs.writeTextFile` capability.\\nAllows the agent to create or modify files within the client's environment.\\n\\nSee protocol docs: [Client](https://agentclientprotocol.com/protocol/overview#client)\","]
#[doc = "              \"allOf\": ["]
#[doc = "                {"]
#[doc = "                  \"$ref\": \"#/$defs/WriteTextFileRequest\""]
#[doc = "                }"]
#[doc = "              ]"]
#[doc = "            },"]
#[doc = "            {"]
#[doc = "              \"title\": \"ReadTextFileRequest\","]
#[doc = "              \"description\": \"Reads content from a text file in the client's file system.\\n\\nOnly available if the client advertises the `fs.readTextFile` capability.\\nAllows the agent to access file contents within the client's environment.\\n\\nSee protocol docs: [Client](https://agentclientprotocol.com/protocol/overview#client)\","]
#[doc = "              \"allOf\": ["]
#[doc = "                {"]
#[doc = "                  \"$ref\": \"#/$defs/ReadTextFileRequest\""]
#[doc = "                }"]
#[doc = "              ]"]
#[doc = "            },"]
#[doc = "            {"]
#[doc = "              \"title\": \"RequestPermissionRequest\","]
#[doc = "              \"description\": \"Requests permission from the user for a tool call operation.\\n\\nCalled by the agent when it needs user authorization before executing\\na potentially sensitive operation. The client should present the options\\nto the user and return their decision.\\n\\nIf the client cancels the prompt turn via `session/cancel`, it MUST\\nrespond to this request with `RequestPermissionOutcome::Cancelled`.\\n\\nSee protocol docs: [Requesting Permission](https://agentclientprotocol.com/protocol/tool-calls#requesting-permission)\","]
#[doc = "              \"allOf\": ["]
#[doc = "                {"]
#[doc = "                  \"$ref\": \"#/$defs/RequestPermissionRequest\""]
#[doc = "                }"]
#[doc = "              ]"]
#[doc = "            },"]
#[doc = "            {"]
#[doc = "              \"title\": \"CreateTerminalRequest\","]
#[doc = "              \"description\": \"Executes a command in a new terminal\\n\\nOnly available if the `terminal` Client capability is set to `true`.\\n\\nReturns a `TerminalId` that can be used with other terminal methods\\nto get the current output, wait for exit, and kill the command.\\n\\nThe `TerminalId` can also be used to embed the terminal in a tool call\\nby using the `ToolCallContent::Terminal` variant.\\n\\nThe Agent is responsible for releasing the terminal by using the `terminal/release`\\nmethod.\\n\\nSee protocol docs: [Terminals](https://agentclientprotocol.com/protocol/terminals)\","]
#[doc = "              \"allOf\": ["]
#[doc = "                {"]
#[doc = "                  \"$ref\": \"#/$defs/CreateTerminalRequest\""]
#[doc = "                }"]
#[doc = "              ]"]
#[doc = "            },"]
#[doc = "            {"]
#[doc = "              \"title\": \"TerminalOutputRequest\","]
#[doc = "              \"description\": \"Gets the terminal output and exit status\\n\\nReturns the current content in the terminal without waiting for the command to exit.\\nIf the command has already exited, the exit status is included.\\n\\nSee protocol docs: [Terminals](https://agentclientprotocol.com/protocol/terminals)\","]
#[doc = "              \"allOf\": ["]
#[doc = "                {"]
#[doc = "                  \"$ref\": \"#/$defs/TerminalOutputRequest\""]
#[doc = "                }"]
#[doc = "              ]"]
#[doc = "            },"]
#[doc = "            {"]
#[doc = "              \"title\": \"ReleaseTerminalRequest\","]
#[doc = "              \"description\": \"Releases a terminal\\n\\nThe command is killed if it hasn't exited yet. Use `terminal/wait_for_exit`\\nto wait for the command to exit before releasing the terminal.\\n\\nAfter release, the `TerminalId` can no longer be used with other `terminal/*` methods,\\nbut tool calls that already contain it, continue to display its output.\\n\\nThe `terminal/kill` method can be used to terminate the command without releasing\\nthe terminal, allowing the Agent to call `terminal/output` and other methods.\\n\\nSee protocol docs: [Terminals](https://agentclientprotocol.com/protocol/terminals)\","]
#[doc = "              \"allOf\": ["]
#[doc = "                {"]
#[doc = "                  \"$ref\": \"#/$defs/ReleaseTerminalRequest\""]
#[doc = "                }"]
#[doc = "              ]"]
#[doc = "            },"]
#[doc = "            {"]
#[doc = "              \"title\": \"WaitForTerminalExitRequest\","]
#[doc = "              \"description\": \"Waits for the terminal command to exit and return its exit status\\n\\nSee protocol docs: [Terminals](https://agentclientprotocol.com/protocol/terminals)\","]
#[doc = "              \"allOf\": ["]
#[doc = "                {"]
#[doc = "                  \"$ref\": \"#/$defs/WaitForTerminalExitRequest\""]
#[doc = "                }"]
#[doc = "              ]"]
#[doc = "            },"]
#[doc = "            {"]
#[doc = "              \"title\": \"KillTerminalRequest\","]
#[doc = "              \"description\": \"Kills the terminal command without releasing the terminal\\n\\nWhile `terminal/release` will also kill the command, this method will keep\\nthe `TerminalId` valid so it can be used with other methods.\\n\\nThis method can be helpful when implementing command timeouts which terminate\\nthe command as soon as elapsed, and then get the final output so it can be sent\\nto the model.\\n\\nNote: Call `terminal/release` when `TerminalId` is no longer needed.\\n\\nSee protocol docs: [Terminals](https://agentclientprotocol.com/protocol/terminals)\","]
#[doc = "              \"allOf\": ["]
#[doc = "                {"]
#[doc = "                  \"$ref\": \"#/$defs/KillTerminalRequest\""]
#[doc = "                }"]
#[doc = "              ]"]
#[doc = "            },"]
#[doc = "            {"]
#[doc = "              \"title\": \"ExtMethodRequest\","]
#[doc = "              \"description\": \"Handles extension method requests from the agent.\\n\\nAllows the Agent to send an arbitrary request that is not part of the ACP spec.\\nExtension methods provide a way to add custom functionality while maintaining\\nprotocol compatibility.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "              \"allOf\": ["]
#[doc = "                {"]
#[doc = "                  \"$ref\": \"#/$defs/ExtRequest\""]
#[doc = "                }"]
#[doc = "              ]"]
#[doc = "            }"]
#[doc = "          ]"]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"type\": \"null\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"x-docs-ignore\": true"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct AgentRequest {
    #[doc = "The request id used to correlate the matching response."]
    pub id: RequestId,
    #[doc = "The method name to invoke."]
    pub method: ::std::string::String,
    #[doc = "Method-specific request parameters."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub params: ::std::option::Option<AgentRequestParams>,
}
impl AgentRequest {
    pub fn builder() -> builder::AgentRequest {
        Default::default()
    }
}
#[doc = "All possible requests that an agent can send to a client.\n\nThis enum is used internally for routing RPC requests. You typically won't need\nto use this directly.\n\nThis enum encompasses all method calls from agent to client."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"All possible requests that an agent can send to a client.\\n\\nThis enum is used internally for routing RPC requests. You typically won't need\\nto use this directly.\\n\\nThis enum encompasses all method calls from agent to client.\","]
#[doc = "  \"anyOf\": ["]
#[doc = "    {"]
#[doc = "      \"title\": \"WriteTextFileRequest\","]
#[doc = "      \"description\": \"Writes content to a text file in the client's file system.\\n\\nOnly available if the client advertises the `fs.writeTextFile` capability.\\nAllows the agent to create or modify files within the client's environment.\\n\\nSee protocol docs: [Client](https://agentclientprotocol.com/protocol/overview#client)\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/WriteTextFileRequest\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"ReadTextFileRequest\","]
#[doc = "      \"description\": \"Reads content from a text file in the client's file system.\\n\\nOnly available if the client advertises the `fs.readTextFile` capability.\\nAllows the agent to access file contents within the client's environment.\\n\\nSee protocol docs: [Client](https://agentclientprotocol.com/protocol/overview#client)\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/ReadTextFileRequest\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"RequestPermissionRequest\","]
#[doc = "      \"description\": \"Requests permission from the user for a tool call operation.\\n\\nCalled by the agent when it needs user authorization before executing\\na potentially sensitive operation. The client should present the options\\nto the user and return their decision.\\n\\nIf the client cancels the prompt turn via `session/cancel`, it MUST\\nrespond to this request with `RequestPermissionOutcome::Cancelled`.\\n\\nSee protocol docs: [Requesting Permission](https://agentclientprotocol.com/protocol/tool-calls#requesting-permission)\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/RequestPermissionRequest\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"CreateTerminalRequest\","]
#[doc = "      \"description\": \"Executes a command in a new terminal\\n\\nOnly available if the `terminal` Client capability is set to `true`.\\n\\nReturns a `TerminalId` that can be used with other terminal methods\\nto get the current output, wait for exit, and kill the command.\\n\\nThe `TerminalId` can also be used to embed the terminal in a tool call\\nby using the `ToolCallContent::Terminal` variant.\\n\\nThe Agent is responsible for releasing the terminal by using the `terminal/release`\\nmethod.\\n\\nSee protocol docs: [Terminals](https://agentclientprotocol.com/protocol/terminals)\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/CreateTerminalRequest\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"TerminalOutputRequest\","]
#[doc = "      \"description\": \"Gets the terminal output and exit status\\n\\nReturns the current content in the terminal without waiting for the command to exit.\\nIf the command has already exited, the exit status is included.\\n\\nSee protocol docs: [Terminals](https://agentclientprotocol.com/protocol/terminals)\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/TerminalOutputRequest\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"ReleaseTerminalRequest\","]
#[doc = "      \"description\": \"Releases a terminal\\n\\nThe command is killed if it hasn't exited yet. Use `terminal/wait_for_exit`\\nto wait for the command to exit before releasing the terminal.\\n\\nAfter release, the `TerminalId` can no longer be used with other `terminal/*` methods,\\nbut tool calls that already contain it, continue to display its output.\\n\\nThe `terminal/kill` method can be used to terminate the command without releasing\\nthe terminal, allowing the Agent to call `terminal/output` and other methods.\\n\\nSee protocol docs: [Terminals](https://agentclientprotocol.com/protocol/terminals)\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/ReleaseTerminalRequest\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"WaitForTerminalExitRequest\","]
#[doc = "      \"description\": \"Waits for the terminal command to exit and return its exit status\\n\\nSee protocol docs: [Terminals](https://agentclientprotocol.com/protocol/terminals)\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/WaitForTerminalExitRequest\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"KillTerminalRequest\","]
#[doc = "      \"description\": \"Kills the terminal command without releasing the terminal\\n\\nWhile `terminal/release` will also kill the command, this method will keep\\nthe `TerminalId` valid so it can be used with other methods.\\n\\nThis method can be helpful when implementing command timeouts which terminate\\nthe command as soon as elapsed, and then get the final output so it can be sent\\nto the model.\\n\\nNote: Call `terminal/release` when `TerminalId` is no longer needed.\\n\\nSee protocol docs: [Terminals](https://agentclientprotocol.com/protocol/terminals)\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/KillTerminalRequest\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"ExtMethodRequest\","]
#[doc = "      \"description\": \"Handles extension method requests from the agent.\\n\\nAllows the Agent to send an arbitrary request that is not part of the ACP spec.\\nExtension methods provide a way to add custom functionality while maintaining\\nprotocol compatibility.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/ExtRequest\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct AgentRequestParams {
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_0: ::std::option::Option<WriteTextFileRequest>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_1: ::std::option::Option<ReadTextFileRequest>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_2: ::std::option::Option<RequestPermissionRequest>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_3: ::std::option::Option<CreateTerminalRequest>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_4: ::std::option::Option<TerminalOutputRequest>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_5: ::std::option::Option<ReleaseTerminalRequest>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_6: ::std::option::Option<WaitForTerminalExitRequest>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_7: ::std::option::Option<KillTerminalRequest>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_8: ::std::option::Option<ExtRequest>,
}
impl ::std::default::Default for AgentRequestParams {
    fn default() -> Self {
        Self {
            subtype_0: Default::default(),
            subtype_1: Default::default(),
            subtype_2: Default::default(),
            subtype_3: Default::default(),
            subtype_4: Default::default(),
            subtype_5: Default::default(),
            subtype_6: Default::default(),
            subtype_7: Default::default(),
            subtype_8: Default::default(),
        }
    }
}
impl AgentRequestParams {
    pub fn builder() -> builder::AgentRequestParams {
        Default::default()
    }
}
#[doc = "A JSON-RPC response object."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"A JSON-RPC response object.\","]
#[doc = "  \"anyOf\": ["]
#[doc = "    {"]
#[doc = "      \"title\": \"Result\","]
#[doc = "      \"description\": \"A successful JSON-RPC response.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"required\": ["]
#[doc = "        \"id\","]
#[doc = "        \"result\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"id\": {"]
#[doc = "          \"description\": \"The id of the request this response answers.\","]
#[doc = "          \"allOf\": ["]
#[doc = "            {"]
#[doc = "              \"$ref\": \"#/$defs/RequestId\""]
#[doc = "            }"]
#[doc = "          ]"]
#[doc = "        },"]
#[doc = "        \"result\": {"]
#[doc = "          \"description\": \"Method-specific response data.\","]
#[doc = "          \"anyOf\": ["]
#[doc = "            {"]
#[doc = "              \"title\": \"InitializeResponse\","]
#[doc = "              \"description\": \"Successful result returned for a `initialize` request.\","]
#[doc = "              \"allOf\": ["]
#[doc = "                {"]
#[doc = "                  \"$ref\": \"#/$defs/InitializeResponse\""]
#[doc = "                }"]
#[doc = "              ]"]
#[doc = "            },"]
#[doc = "            {"]
#[doc = "              \"title\": \"AuthenticateResponse\","]
#[doc = "              \"description\": \"Successful result returned for a `authenticate` request.\","]
#[doc = "              \"allOf\": ["]
#[doc = "                {"]
#[doc = "                  \"$ref\": \"#/$defs/AuthenticateResponse\""]
#[doc = "                }"]
#[doc = "              ]"]
#[doc = "            },"]
#[doc = "            {"]
#[doc = "              \"title\": \"LogoutResponse\","]
#[doc = "              \"description\": \"Successful result returned for a `logout` request.\","]
#[doc = "              \"allOf\": ["]
#[doc = "                {"]
#[doc = "                  \"$ref\": \"#/$defs/LogoutResponse\""]
#[doc = "                }"]
#[doc = "              ]"]
#[doc = "            },"]
#[doc = "            {"]
#[doc = "              \"title\": \"NewSessionResponse\","]
#[doc = "              \"description\": \"Successful result returned for a `session/new` request.\","]
#[doc = "              \"allOf\": ["]
#[doc = "                {"]
#[doc = "                  \"$ref\": \"#/$defs/NewSessionResponse\""]
#[doc = "                }"]
#[doc = "              ]"]
#[doc = "            },"]
#[doc = "            {"]
#[doc = "              \"title\": \"LoadSessionResponse\","]
#[doc = "              \"description\": \"Successful result returned for a `session/load` request.\","]
#[doc = "              \"allOf\": ["]
#[doc = "                {"]
#[doc = "                  \"$ref\": \"#/$defs/LoadSessionResponse\""]
#[doc = "                }"]
#[doc = "              ]"]
#[doc = "            },"]
#[doc = "            {"]
#[doc = "              \"title\": \"ListSessionsResponse\","]
#[doc = "              \"description\": \"Successful result returned for a `session/list` request.\","]
#[doc = "              \"allOf\": ["]
#[doc = "                {"]
#[doc = "                  \"$ref\": \"#/$defs/ListSessionsResponse\""]
#[doc = "                }"]
#[doc = "              ]"]
#[doc = "            },"]
#[doc = "            {"]
#[doc = "              \"title\": \"DeleteSessionResponse\","]
#[doc = "              \"description\": \"Successful result returned for a `session/delete` request.\","]
#[doc = "              \"allOf\": ["]
#[doc = "                {"]
#[doc = "                  \"$ref\": \"#/$defs/DeleteSessionResponse\""]
#[doc = "                }"]
#[doc = "              ]"]
#[doc = "            },"]
#[doc = "            {"]
#[doc = "              \"title\": \"ResumeSessionResponse\","]
#[doc = "              \"description\": \"Successful result returned for a `session/resume` request.\","]
#[doc = "              \"allOf\": ["]
#[doc = "                {"]
#[doc = "                  \"$ref\": \"#/$defs/ResumeSessionResponse\""]
#[doc = "                }"]
#[doc = "              ]"]
#[doc = "            },"]
#[doc = "            {"]
#[doc = "              \"title\": \"CloseSessionResponse\","]
#[doc = "              \"description\": \"Successful result returned for a `session/close` request.\","]
#[doc = "              \"allOf\": ["]
#[doc = "                {"]
#[doc = "                  \"$ref\": \"#/$defs/CloseSessionResponse\""]
#[doc = "                }"]
#[doc = "              ]"]
#[doc = "            },"]
#[doc = "            {"]
#[doc = "              \"title\": \"SetSessionModeResponse\","]
#[doc = "              \"description\": \"Successful result returned for a `session/set_mode` request.\","]
#[doc = "              \"allOf\": ["]
#[doc = "                {"]
#[doc = "                  \"$ref\": \"#/$defs/SetSessionModeResponse\""]
#[doc = "                }"]
#[doc = "              ]"]
#[doc = "            },"]
#[doc = "            {"]
#[doc = "              \"title\": \"SetSessionConfigOptionResponse\","]
#[doc = "              \"description\": \"Successful result returned for a `session/set_config_option` request.\","]
#[doc = "              \"allOf\": ["]
#[doc = "                {"]
#[doc = "                  \"$ref\": \"#/$defs/SetSessionConfigOptionResponse\""]
#[doc = "                }"]
#[doc = "              ]"]
#[doc = "            },"]
#[doc = "            {"]
#[doc = "              \"title\": \"PromptResponse\","]
#[doc = "              \"description\": \"Successful result returned for a `session/prompt` request.\","]
#[doc = "              \"allOf\": ["]
#[doc = "                {"]
#[doc = "                  \"$ref\": \"#/$defs/PromptResponse\""]
#[doc = "                }"]
#[doc = "              ]"]
#[doc = "            },"]
#[doc = "            {"]
#[doc = "              \"title\": \"ExtMethodResponse\","]
#[doc = "              \"description\": \"Successful result returned by an extension method outside the core ACP method set.\","]
#[doc = "              \"allOf\": ["]
#[doc = "                {"]
#[doc = "                  \"$ref\": \"#/$defs/ExtResponse\""]
#[doc = "                }"]
#[doc = "              ]"]
#[doc = "            }"]
#[doc = "          ]"]
#[doc = "        }"]
#[doc = "      }"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"Error\","]
#[doc = "      \"description\": \"A failed JSON-RPC response.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"required\": ["]
#[doc = "        \"error\","]
#[doc = "        \"id\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"error\": {"]
#[doc = "          \"description\": \"Method-specific error data.\","]
#[doc = "          \"allOf\": ["]
#[doc = "            {"]
#[doc = "              \"$ref\": \"#/$defs/Error\""]
#[doc = "            }"]
#[doc = "          ]"]
#[doc = "        },"]
#[doc = "        \"id\": {"]
#[doc = "          \"description\": \"The id of the request this response answers.\","]
#[doc = "          \"allOf\": ["]
#[doc = "            {"]
#[doc = "              \"$ref\": \"#/$defs/RequestId\""]
#[doc = "            }"]
#[doc = "          ]"]
#[doc = "        }"]
#[doc = "      }"]
#[doc = "    }"]
#[doc = "  ],"]
#[doc = "  \"x-docs-ignore\": true"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
#[serde(untagged)]
pub enum AgentResponse {
    Result {
        #[doc = "The id of the request this response answers."]
        id: RequestId,
        #[doc = "Method-specific response data."]
        result: ResultResult,
    },
    Error {
        #[doc = "Method-specific error data."]
        error: Error,
        #[doc = "The id of the request this response answers."]
        id: RequestId,
    },
}
#[doc = "`AgentVariant0Jsonrpc`"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"type\": \"string\","]
#[doc = "  \"enum\": ["]
#[doc = "    \"2.0\""]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(
    :: serde :: Deserialize,
    :: serde :: Serialize,
    Clone,
    Copy,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
)]
pub enum AgentVariant0Jsonrpc {
    #[serde(rename = "2.0")]
    X20,
}
impl ::std::fmt::Display for AgentVariant0Jsonrpc {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        match *self {
            Self::X20 => f.write_str("2.0"),
        }
    }
}
impl ::std::str::FromStr for AgentVariant0Jsonrpc {
    type Err = self::error::ConversionError;
    fn from_str(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        match value {
            "2.0" => Ok(Self::X20),
            _ => Err("invalid value".into()),
        }
    }
}
impl ::std::convert::TryFrom<&str> for AgentVariant0Jsonrpc {
    type Error = self::error::ConversionError;
    fn try_from(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<&::std::string::String> for AgentVariant0Jsonrpc {
    type Error = self::error::ConversionError;
    fn try_from(
        value: &::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<::std::string::String> for AgentVariant0Jsonrpc {
    type Error = self::error::ConversionError;
    fn try_from(
        value: ::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
#[doc = "All possible requests that an agent can send to a client.\n\nThis enum is used internally for routing RPC requests. You typically won't need\nto use this directly.\n\nThis enum encompasses all method calls from agent to client."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"All possible requests that an agent can send to a client.\\n\\nThis enum is used internally for routing RPC requests. You typically won't need\\nto use this directly.\\n\\nThis enum encompasses all method calls from agent to client.\","]
#[doc = "  \"anyOf\": ["]
#[doc = "    {"]
#[doc = "      \"title\": \"WriteTextFileRequest\","]
#[doc = "      \"description\": \"Writes content to a text file in the client's file system.\\n\\nOnly available if the client advertises the `fs.writeTextFile` capability.\\nAllows the agent to create or modify files within the client's environment.\\n\\nSee protocol docs: [Client](https://agentclientprotocol.com/protocol/overview#client)\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/WriteTextFileRequest\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"ReadTextFileRequest\","]
#[doc = "      \"description\": \"Reads content from a text file in the client's file system.\\n\\nOnly available if the client advertises the `fs.readTextFile` capability.\\nAllows the agent to access file contents within the client's environment.\\n\\nSee protocol docs: [Client](https://agentclientprotocol.com/protocol/overview#client)\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/ReadTextFileRequest\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"RequestPermissionRequest\","]
#[doc = "      \"description\": \"Requests permission from the user for a tool call operation.\\n\\nCalled by the agent when it needs user authorization before executing\\na potentially sensitive operation. The client should present the options\\nto the user and return their decision.\\n\\nIf the client cancels the prompt turn via `session/cancel`, it MUST\\nrespond to this request with `RequestPermissionOutcome::Cancelled`.\\n\\nSee protocol docs: [Requesting Permission](https://agentclientprotocol.com/protocol/tool-calls#requesting-permission)\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/RequestPermissionRequest\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"CreateTerminalRequest\","]
#[doc = "      \"description\": \"Executes a command in a new terminal\\n\\nOnly available if the `terminal` Client capability is set to `true`.\\n\\nReturns a `TerminalId` that can be used with other terminal methods\\nto get the current output, wait for exit, and kill the command.\\n\\nThe `TerminalId` can also be used to embed the terminal in a tool call\\nby using the `ToolCallContent::Terminal` variant.\\n\\nThe Agent is responsible for releasing the terminal by using the `terminal/release`\\nmethod.\\n\\nSee protocol docs: [Terminals](https://agentclientprotocol.com/protocol/terminals)\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/CreateTerminalRequest\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"TerminalOutputRequest\","]
#[doc = "      \"description\": \"Gets the terminal output and exit status\\n\\nReturns the current content in the terminal without waiting for the command to exit.\\nIf the command has already exited, the exit status is included.\\n\\nSee protocol docs: [Terminals](https://agentclientprotocol.com/protocol/terminals)\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/TerminalOutputRequest\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"ReleaseTerminalRequest\","]
#[doc = "      \"description\": \"Releases a terminal\\n\\nThe command is killed if it hasn't exited yet. Use `terminal/wait_for_exit`\\nto wait for the command to exit before releasing the terminal.\\n\\nAfter release, the `TerminalId` can no longer be used with other `terminal/*` methods,\\nbut tool calls that already contain it, continue to display its output.\\n\\nThe `terminal/kill` method can be used to terminate the command without releasing\\nthe terminal, allowing the Agent to call `terminal/output` and other methods.\\n\\nSee protocol docs: [Terminals](https://agentclientprotocol.com/protocol/terminals)\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/ReleaseTerminalRequest\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"WaitForTerminalExitRequest\","]
#[doc = "      \"description\": \"Waits for the terminal command to exit and return its exit status\\n\\nSee protocol docs: [Terminals](https://agentclientprotocol.com/protocol/terminals)\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/WaitForTerminalExitRequest\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"KillTerminalRequest\","]
#[doc = "      \"description\": \"Kills the terminal command without releasing the terminal\\n\\nWhile `terminal/release` will also kill the command, this method will keep\\nthe `TerminalId` valid so it can be used with other methods.\\n\\nThis method can be helpful when implementing command timeouts which terminate\\nthe command as soon as elapsed, and then get the final output so it can be sent\\nto the model.\\n\\nNote: Call `terminal/release` when `TerminalId` is no longer needed.\\n\\nSee protocol docs: [Terminals](https://agentclientprotocol.com/protocol/terminals)\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/KillTerminalRequest\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"ExtMethodRequest\","]
#[doc = "      \"description\": \"Handles extension method requests from the agent.\\n\\nAllows the Agent to send an arbitrary request that is not part of the ACP spec.\\nExtension methods provide a way to add custom functionality while maintaining\\nprotocol compatibility.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/ExtRequest\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct AgentVariant0Params {
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_0: ::std::option::Option<WriteTextFileRequest>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_1: ::std::option::Option<ReadTextFileRequest>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_2: ::std::option::Option<RequestPermissionRequest>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_3: ::std::option::Option<CreateTerminalRequest>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_4: ::std::option::Option<TerminalOutputRequest>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_5: ::std::option::Option<ReleaseTerminalRequest>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_6: ::std::option::Option<WaitForTerminalExitRequest>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_7: ::std::option::Option<KillTerminalRequest>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_8: ::std::option::Option<ExtRequest>,
}
impl ::std::default::Default for AgentVariant0Params {
    fn default() -> Self {
        Self {
            subtype_0: Default::default(),
            subtype_1: Default::default(),
            subtype_2: Default::default(),
            subtype_3: Default::default(),
            subtype_4: Default::default(),
            subtype_5: Default::default(),
            subtype_6: Default::default(),
            subtype_7: Default::default(),
            subtype_8: Default::default(),
        }
    }
}
impl AgentVariant0Params {
    pub fn builder() -> builder::AgentVariant0Params {
        Default::default()
    }
}
#[doc = "`AgentVariant1`"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"allOf\": ["]
#[doc = "    {"]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"required\": ["]
#[doc = "        \"jsonrpc\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"jsonrpc\": {"]
#[doc = "          \"type\": \"string\","]
#[doc = "          \"enum\": ["]
#[doc = "            \"2.0\""]
#[doc = "          ]"]
#[doc = "        }"]
#[doc = "      }"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"Response\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/AgentResponse\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"not\": {"]
#[doc = "        \"title\": \"Request\","]
#[doc = "        \"allOf\": ["]
#[doc = "          {"]
#[doc = "            \"$ref\": \"#/$defs/AgentRequest\""]
#[doc = "          }"]
#[doc = "        ]"]
#[doc = "      }"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"not\": {"]
#[doc = "        \"title\": \"Notification\","]
#[doc = "        \"allOf\": ["]
#[doc = "          {"]
#[doc = "            \"$ref\": \"#/$defs/AgentNotification\""]
#[doc = "          }"]
#[doc = "        ]"]
#[doc = "      }"]
#[doc = "    }"]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(
    :: serde :: Deserialize,
    :: serde :: Serialize,
    Clone,
    Copy,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
)]
#[serde(deny_unknown_fields)]
pub enum AgentVariant1 {}
#[doc = "`AgentVariant2Jsonrpc`"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"type\": \"string\","]
#[doc = "  \"enum\": ["]
#[doc = "    \"2.0\""]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(
    :: serde :: Deserialize,
    :: serde :: Serialize,
    Clone,
    Copy,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
)]
pub enum AgentVariant2Jsonrpc {
    #[serde(rename = "2.0")]
    X20,
}
impl ::std::fmt::Display for AgentVariant2Jsonrpc {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        match *self {
            Self::X20 => f.write_str("2.0"),
        }
    }
}
impl ::std::str::FromStr for AgentVariant2Jsonrpc {
    type Err = self::error::ConversionError;
    fn from_str(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        match value {
            "2.0" => Ok(Self::X20),
            _ => Err("invalid value".into()),
        }
    }
}
impl ::std::convert::TryFrom<&str> for AgentVariant2Jsonrpc {
    type Error = self::error::ConversionError;
    fn try_from(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<&::std::string::String> for AgentVariant2Jsonrpc {
    type Error = self::error::ConversionError;
    fn try_from(
        value: &::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<::std::string::String> for AgentVariant2Jsonrpc {
    type Error = self::error::ConversionError;
    fn try_from(
        value: ::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
#[doc = "All possible notifications that an agent can send to a client.\n\nThis enum is used internally for routing RPC notifications. You typically won't need\nto use this directly.\n\nNotifications do not expect a response."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"All possible notifications that an agent can send to a client.\\n\\nThis enum is used internally for routing RPC notifications. You typically won't need\\nto use this directly.\\n\\nNotifications do not expect a response.\","]
#[doc = "  \"anyOf\": ["]
#[doc = "    {"]
#[doc = "      \"title\": \"SessionNotification\","]
#[doc = "      \"description\": \"Handles session update notifications from the agent.\\n\\nThis is a notification endpoint (no response expected) that receives\\nreal-time updates about session progress, including message chunks,\\ntool calls, and execution plans.\\n\\nNote: Clients SHOULD continue accepting tool call updates even after\\nsending a `session/cancel` notification, as the agent may send final\\nupdates before responding with the cancelled stop reason.\\n\\nSee protocol docs: [Agent Reports Output](https://agentclientprotocol.com/protocol/prompt-turn#3-agent-reports-output)\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/SessionNotification\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"ExtNotification\","]
#[doc = "      \"description\": \"Handles extension notifications from the agent.\\n\\nAllows the Agent to send an arbitrary notification that is not part of the ACP spec.\\nExtension notifications provide a way to send one-way messages for custom functionality\\nwhile maintaining protocol compatibility.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/ExtNotification\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct AgentVariant2Params {
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_0: ::std::option::Option<SessionNotification>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_1: ::std::option::Option<ExtNotification>,
}
impl ::std::default::Default for AgentVariant2Params {
    fn default() -> Self {
        Self {
            subtype_0: Default::default(),
            subtype_1: Default::default(),
        }
    }
}
impl AgentVariant2Params {
    pub fn builder() -> builder::AgentVariant2Params {
        Default::default()
    }
}
#[doc = "Optional annotations for the client. The client can use annotations to inform how objects are used or displayed"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Optional annotations for the client. The client can use annotations to inform how objects are used or displayed\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"audience\": {"]
#[doc = "      \"description\": \"Intended recipients for this content, such as the user or assistant.\","]
#[doc = "      \"type\": ["]
#[doc = "        \"array\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"items\": {"]
#[doc = "        \"$ref\": \"#/$defs/Role\""]
#[doc = "      },"]
#[doc = "      \"x-deserialize-default-on-error\": true,"]
#[doc = "      \"x-deserialize-skip-invalid-items\": true"]
#[doc = "    },"]
#[doc = "    \"lastModified\": {"]
#[doc = "      \"description\": \"Timestamp indicating when the underlying resource was last modified.\","]
#[doc = "      \"type\": ["]
#[doc = "        \"string\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"priority\": {"]
#[doc = "      \"description\": \"Relative importance of this content when clients choose what to surface.\","]
#[doc = "      \"type\": ["]
#[doc = "        \"number\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"format\": \"double\","]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct Annotations {
    #[doc = "Intended recipients for this content, such as the user or assistant."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub audience: ::std::option::Option<::std::vec::Vec<Role>>,
    #[doc = "Timestamp indicating when the underlying resource was last modified."]
    #[serde(
        rename = "lastModified",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub last_modified: ::std::option::Option<::std::string::String>,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "Relative importance of this content when clients choose what to surface."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub priority: ::std::option::Option<f64>,
}
impl ::std::default::Default for Annotations {
    fn default() -> Self {
        Self {
            audience: Default::default(),
            last_modified: Default::default(),
            meta: Default::default(),
            priority: Default::default(),
        }
    }
}
impl Annotations {
    pub fn builder() -> builder::Annotations {
        Default::default()
    }
}
#[doc = "Audio provided to or from an LLM."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Audio provided to or from an LLM.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"data\","]
#[doc = "    \"mimeType\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"annotations\": {"]
#[doc = "      \"description\": \"Optional annotations that help clients decide how to display or route this content.\","]
#[doc = "      \"anyOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/Annotations\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"type\": \"null\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"data\": {"]
#[doc = "      \"description\": \"Base64-encoded media payload.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"mimeType\": {"]
#[doc = "      \"description\": \"MIME type describing the encoded media payload.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct AudioContent {
    #[doc = "Optional annotations that help clients decide how to display or route this content."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub annotations: ::std::option::Option<Annotations>,
    #[doc = "Base64-encoded media payload."]
    pub data: ::std::string::String,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "MIME type describing the encoded media payload."]
    #[serde(rename = "mimeType")]
    pub mime_type: ::std::string::String,
}
impl AudioContent {
    pub fn builder() -> builder::AudioContent {
        Default::default()
    }
}
#[doc = "Describes an available authentication method.\n\nThe `type` field acts as the discriminator in the serialized JSON form.\nWhen no `type` is present, the method is treated as `agent`."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Describes an available authentication method.\\n\\nThe `type` field acts as the discriminator in the serialized JSON form.\\nWhen no `type` is present, the method is treated as `agent`.\","]
#[doc = "  \"anyOf\": ["]
#[doc = "    {"]
#[doc = "      \"title\": \"agent\","]
#[doc = "      \"description\": \"Agent handles authentication itself.\\n\\nThis is the default when no `type` is specified.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/AuthMethodAgent\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
#[serde(transparent)]
pub struct AuthMethod(pub AuthMethodAgent);
impl ::std::ops::Deref for AuthMethod {
    type Target = AuthMethodAgent;
    fn deref(&self) -> &AuthMethodAgent {
        &self.0
    }
}
impl ::std::convert::From<AuthMethod> for AuthMethodAgent {
    fn from(value: AuthMethod) -> Self {
        value.0
    }
}
impl ::std::convert::From<AuthMethodAgent> for AuthMethod {
    fn from(value: AuthMethodAgent) -> Self {
        Self(value)
    }
}
#[doc = "Agent handles authentication itself.\n\nThis is the default authentication method type."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Agent handles authentication itself.\\n\\nThis is the default authentication method type.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"id\","]
#[doc = "    \"name\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"description\": {"]
#[doc = "      \"description\": \"Optional description providing more details about this authentication method.\","]
#[doc = "      \"type\": ["]
#[doc = "        \"string\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"id\": {"]
#[doc = "      \"description\": \"Unique identifier for this authentication method.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/AuthMethodId\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    \"name\": {"]
#[doc = "      \"description\": \"Human-readable name of the authentication method.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct AuthMethodAgent {
    #[doc = "Optional description providing more details about this authentication method."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub description: ::std::option::Option<::std::string::String>,
    #[doc = "Unique identifier for this authentication method."]
    pub id: AuthMethodId,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "Human-readable name of the authentication method."]
    pub name: ::std::string::String,
}
impl AuthMethodAgent {
    pub fn builder() -> builder::AuthMethodAgent {
        Default::default()
    }
}
#[doc = "Typed identifier used for auth method values on the wire."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Typed identifier used for auth method values on the wire.\","]
#[doc = "  \"type\": \"string\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(
    :: serde :: Deserialize,
    :: serde :: Serialize,
    Clone,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
)]
#[serde(transparent)]
pub struct AuthMethodId(pub ::std::string::String);
impl ::std::ops::Deref for AuthMethodId {
    type Target = ::std::string::String;
    fn deref(&self) -> &::std::string::String {
        &self.0
    }
}
impl ::std::convert::From<AuthMethodId> for ::std::string::String {
    fn from(value: AuthMethodId) -> Self {
        value.0
    }
}
impl ::std::convert::From<::std::string::String> for AuthMethodId {
    fn from(value: ::std::string::String) -> Self {
        Self(value)
    }
}
impl ::std::str::FromStr for AuthMethodId {
    type Err = ::std::convert::Infallible;
    fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
        Ok(Self(value.to_string()))
    }
}
impl ::std::fmt::Display for AuthMethodId {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        self.0.fmt(f)
    }
}
#[doc = "Request parameters for the authenticate method.\n\nSpecifies which authentication method to use."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Request parameters for the authenticate method.\\n\\nSpecifies which authentication method to use.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"methodId\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"methodId\": {"]
#[doc = "      \"description\": \"The ID of the authentication method to use.\\nMust be one of the methods advertised in the initialize response.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/AuthMethodId\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"x-method\": \"authenticate\","]
#[doc = "  \"x-side\": \"agent\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct AuthenticateRequest {
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "The ID of the authentication method to use.\nMust be one of the methods advertised in the initialize response."]
    #[serde(rename = "methodId")]
    pub method_id: AuthMethodId,
}
impl AuthenticateRequest {
    pub fn builder() -> builder::AuthenticateRequest {
        Default::default()
    }
}
#[doc = "Response to the `authenticate` method."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Response to the `authenticate` method.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"x-method\": \"authenticate\","]
#[doc = "  \"x-side\": \"agent\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct AuthenticateResponse {
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
}
impl ::std::default::Default for AuthenticateResponse {
    fn default() -> Self {
        Self {
            meta: Default::default(),
        }
    }
}
impl AuthenticateResponse {
    pub fn builder() -> builder::AuthenticateResponse {
        Default::default()
    }
}
#[doc = "Information about a command."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Information about a command.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"description\","]
#[doc = "    \"name\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"description\": {"]
#[doc = "      \"description\": \"Human-readable description of what the command does.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"input\": {"]
#[doc = "      \"description\": \"Input for the command if required\","]
#[doc = "      \"anyOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/AvailableCommandInput\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"type\": \"null\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"name\": {"]
#[doc = "      \"description\": \"Command name (e.g., `create_plan`, `research_codebase`).\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct AvailableCommand {
    #[doc = "Human-readable description of what the command does."]
    pub description: ::std::string::String,
    #[doc = "Input for the command if required"]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub input: ::std::option::Option<AvailableCommandInput>,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "Command name (e.g., `create_plan`, `research_codebase`)."]
    pub name: ::std::string::String,
}
impl AvailableCommand {
    pub fn builder() -> builder::AvailableCommand {
        Default::default()
    }
}
#[doc = "The input specification for a command."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"The input specification for a command.\","]
#[doc = "  \"anyOf\": ["]
#[doc = "    {"]
#[doc = "      \"title\": \"unstructured\","]
#[doc = "      \"description\": \"All text that was typed after the command name is provided as input.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/UnstructuredCommandInput\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
#[serde(transparent)]
pub struct AvailableCommandInput(pub UnstructuredCommandInput);
impl ::std::ops::Deref for AvailableCommandInput {
    type Target = UnstructuredCommandInput;
    fn deref(&self) -> &UnstructuredCommandInput {
        &self.0
    }
}
impl ::std::convert::From<AvailableCommandInput> for UnstructuredCommandInput {
    fn from(value: AvailableCommandInput) -> Self {
        value.0
    }
}
impl ::std::convert::From<UnstructuredCommandInput> for AvailableCommandInput {
    fn from(value: UnstructuredCommandInput) -> Self {
        Self(value)
    }
}
#[doc = "Available commands are ready or have changed"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Available commands are ready or have changed\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"availableCommands\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"availableCommands\": {"]
#[doc = "      \"description\": \"Commands the agent can execute\","]
#[doc = "      \"type\": \"array\","]
#[doc = "      \"items\": {"]
#[doc = "        \"$ref\": \"#/$defs/AvailableCommand\""]
#[doc = "      },"]
#[doc = "      \"x-deserialize-default-on-error\": true,"]
#[doc = "      \"x-deserialize-skip-invalid-items\": true"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct AvailableCommandsUpdate {
    #[doc = "Commands the agent can execute"]
    #[serde(rename = "availableCommands")]
    pub available_commands: ::std::vec::Vec<AvailableCommand>,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
}
impl AvailableCommandsUpdate {
    pub fn builder() -> builder::AvailableCommandsUpdate {
        Default::default()
    }
}
#[doc = "Binary resource contents."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Binary resource contents.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"blob\","]
#[doc = "    \"uri\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"blob\": {"]
#[doc = "      \"description\": \"Base64-encoded bytes for a binary resource payload.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"mimeType\": {"]
#[doc = "      \"description\": \"MIME type describing the encoded media payload.\","]
#[doc = "      \"type\": ["]
#[doc = "        \"string\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"uri\": {"]
#[doc = "      \"description\": \"URI associated with this resource or media payload.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct BlobResourceContents {
    #[doc = "Base64-encoded bytes for a binary resource payload."]
    pub blob: ::std::string::String,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "MIME type describing the encoded media payload."]
    #[serde(
        rename = "mimeType",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub mime_type: ::std::option::Option<::std::string::String>,
    #[doc = "URI associated with this resource or media payload."]
    pub uri: ::std::string::String,
}
impl BlobResourceContents {
    pub fn builder() -> builder::BlobResourceContents {
        Default::default()
    }
}
#[doc = "Capabilities for boolean session configuration options.\n\nSupplying `{}` means the client supports boolean session configuration options."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Capabilities for boolean session configuration options.\\n\\nSupplying `{}` means the client supports boolean session configuration options.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct BooleanConfigOptionCapabilities {
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
}
impl ::std::default::Default for BooleanConfigOptionCapabilities {
    fn default() -> Self {
        Self {
            meta: Default::default(),
        }
    }
}
impl BooleanConfigOptionCapabilities {
    pub fn builder() -> builder::BooleanConfigOptionCapabilities {
        Default::default()
    }
}
#[doc = "Notification to cancel ongoing operations for a session.\n\nSee protocol docs: [Cancellation](https://agentclientprotocol.com/protocol/prompt-turn#cancellation)"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Notification to cancel ongoing operations for a session.\\n\\nSee protocol docs: [Cancellation](https://agentclientprotocol.com/protocol/prompt-turn#cancellation)\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"sessionId\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"sessionId\": {"]
#[doc = "      \"description\": \"The ID of the session to cancel operations for.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/SessionId\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"x-method\": \"session/cancel\","]
#[doc = "  \"x-side\": \"agent\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct CancelNotification {
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "The ID of the session to cancel operations for."]
    #[serde(rename = "sessionId")]
    pub session_id: SessionId,
}
impl CancelNotification {
    pub fn builder() -> builder::CancelNotification {
        Default::default()
    }
}
#[doc = "Notification to cancel an ongoing request.\n\nSee protocol docs: [Cancellation](https://agentclientprotocol.com/protocol/cancellation)"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Notification to cancel an ongoing request.\\n\\nSee protocol docs: [Cancellation](https://agentclientprotocol.com/protocol/cancellation)\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"requestId\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"requestId\": {"]
#[doc = "      \"description\": \"The ID of the request to cancel.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/RequestId\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"x-method\": \"$/cancel_request\","]
#[doc = "  \"x-side\": \"protocol\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct CancelRequestNotification {
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "The ID of the request to cancel."]
    #[serde(rename = "requestId")]
    pub request_id: RequestId,
}
impl CancelRequestNotification {
    pub fn builder() -> builder::CancelRequestNotification {
        Default::default()
    }
}
#[doc = "A message (request, response, or notification) with `\"jsonrpc\": \"2.0\"` specified as\n[required by JSON-RPC 2.0 Specification][1].\n\n[1]: https://www.jsonrpc.org/specification#compatibility"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"Client\","]
#[doc = "  \"description\": \"A message (request, response, or notification) with `\\\"jsonrpc\\\": \\\"2.0\\\"` specified as\\n[required by JSON-RPC 2.0 Specification][1].\\n\\n[1]: https://www.jsonrpc.org/specification#compatibility\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"anyOf\": ["]
#[doc = "    {"]
#[doc = "      \"title\": \"Request\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/ClientRequest\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"Response\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/ClientResponse\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"Notification\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/ClientNotification\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  ],"]
#[doc = "  \"required\": ["]
#[doc = "    \"jsonrpc\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"jsonrpc\": {"]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"enum\": ["]
#[doc = "        \"2.0\""]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
#[serde(untagged)]
pub enum Client {
    Variant0 {
        #[doc = "The request id used to correlate the matching response."]
        id: RequestId,
        jsonrpc: ClientVariant0Jsonrpc,
        #[doc = "The method name to invoke."]
        method: ::std::string::String,
        #[doc = "Method-specific request parameters."]
        #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
        params: ::std::option::Option<ClientVariant0Params>,
    },
    Variant1(ClientVariant1),
    Variant2 {
        jsonrpc: ClientVariant2Jsonrpc,
        #[doc = "The notification method name."]
        method: ::std::string::String,
        #[doc = "Method-specific notification parameters."]
        #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
        params: ::std::option::Option<ClientVariant2Params>,
    },
}
impl ::std::convert::From<ClientVariant1> for Client {
    fn from(value: ClientVariant1) -> Self {
        Self::Variant1(value)
    }
}
#[doc = "Capabilities supported by the client.\n\nAdvertised during initialization to inform the agent about\navailable features and methods.\n\nSee protocol docs: [Client Capabilities](https://agentclientprotocol.com/protocol/initialization#client-capabilities)"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Capabilities supported by the client.\\n\\nAdvertised during initialization to inform the agent about\\navailable features and methods.\\n\\nSee protocol docs: [Client Capabilities](https://agentclientprotocol.com/protocol/initialization#client-capabilities)\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"fs\": {"]
#[doc = "      \"description\": \"File system capabilities supported by the client.\\nDetermines which file operations the agent can request.\","]
#[doc = "      \"default\": {"]
#[doc = "        \"readTextFile\": false,"]
#[doc = "        \"writeTextFile\": false"]
#[doc = "      },"]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/FileSystemCapabilities\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"session\": {"]
#[doc = "      \"description\": \"Session-related capabilities supported by the client.\\n\\nOptional. Omitted or `null` both mean the client does not advertise any\\nsession-related extensions.\","]
#[doc = "      \"anyOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/ClientSessionCapabilities\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"type\": \"null\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"terminal\": {"]
#[doc = "      \"description\": \"Whether the Client support all `terminal/*` methods.\","]
#[doc = "      \"default\": false,"]
#[doc = "      \"type\": \"boolean\","]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct ClientCapabilities {
    #[doc = "File system capabilities supported by the client.\nDetermines which file operations the agent can request."]
    #[serde(default = "defaults::client_capabilities_fs")]
    pub fs: FileSystemCapabilities,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "Session-related capabilities supported by the client.\n\nOptional. Omitted or `null` both mean the client does not advertise any\nsession-related extensions."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub session: ::std::option::Option<ClientSessionCapabilities>,
    #[doc = "Whether the Client support all `terminal/*` methods."]
    #[serde(default)]
    pub terminal: bool,
}
impl ::std::default::Default for ClientCapabilities {
    fn default() -> Self {
        Self {
            fs: defaults::client_capabilities_fs(),
            meta: Default::default(),
            session: Default::default(),
            terminal: Default::default(),
        }
    }
}
impl ClientCapabilities {
    pub fn builder() -> builder::ClientCapabilities {
        Default::default()
    }
}
#[doc = "A JSON-RPC notification object."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"A JSON-RPC notification object.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"method\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"method\": {"]
#[doc = "      \"description\": \"The notification method name.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"params\": {"]
#[doc = "      \"description\": \"Method-specific notification parameters.\","]
#[doc = "      \"anyOf\": ["]
#[doc = "        {"]
#[doc = "          \"description\": \"All possible notifications that a client can send to an agent.\\n\\nThis enum is used internally for routing RPC notifications. You typically won't need\\nto use this directly.\\n\\nNotifications do not expect a response.\","]
#[doc = "          \"anyOf\": ["]
#[doc = "            {"]
#[doc = "              \"title\": \"CancelNotification\","]
#[doc = "              \"description\": \"Cancels ongoing operations for a session.\\n\\nThis is a notification sent by the client to cancel an ongoing prompt turn.\\n\\nUpon receiving this notification, the Agent SHOULD:\\n- Stop all language model requests as soon as possible\\n- Abort all tool call invocations in progress\\n- Send any pending `session/update` notifications\\n- Respond to the original `session/prompt` request with `StopReason::Cancelled`\\n\\nSee protocol docs: [Cancellation](https://agentclientprotocol.com/protocol/prompt-turn#cancellation)\","]
#[doc = "              \"allOf\": ["]
#[doc = "                {"]
#[doc = "                  \"$ref\": \"#/$defs/CancelNotification\""]
#[doc = "                }"]
#[doc = "              ]"]
#[doc = "            },"]
#[doc = "            {"]
#[doc = "              \"title\": \"ExtNotification\","]
#[doc = "              \"description\": \"Handles extension notifications from the client.\\n\\nExtension notifications provide a way to send one-way messages for custom functionality\\nwhile maintaining protocol compatibility.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "              \"allOf\": ["]
#[doc = "                {"]
#[doc = "                  \"$ref\": \"#/$defs/ExtNotification\""]
#[doc = "                }"]
#[doc = "              ]"]
#[doc = "            }"]
#[doc = "          ]"]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"type\": \"null\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"x-docs-ignore\": true"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct ClientNotification {
    #[doc = "The notification method name."]
    pub method: ::std::string::String,
    #[doc = "Method-specific notification parameters."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub params: ::std::option::Option<ClientNotificationParams>,
}
impl ClientNotification {
    pub fn builder() -> builder::ClientNotification {
        Default::default()
    }
}
#[doc = "All possible notifications that a client can send to an agent.\n\nThis enum is used internally for routing RPC notifications. You typically won't need\nto use this directly.\n\nNotifications do not expect a response."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"All possible notifications that a client can send to an agent.\\n\\nThis enum is used internally for routing RPC notifications. You typically won't need\\nto use this directly.\\n\\nNotifications do not expect a response.\","]
#[doc = "  \"anyOf\": ["]
#[doc = "    {"]
#[doc = "      \"title\": \"CancelNotification\","]
#[doc = "      \"description\": \"Cancels ongoing operations for a session.\\n\\nThis is a notification sent by the client to cancel an ongoing prompt turn.\\n\\nUpon receiving this notification, the Agent SHOULD:\\n- Stop all language model requests as soon as possible\\n- Abort all tool call invocations in progress\\n- Send any pending `session/update` notifications\\n- Respond to the original `session/prompt` request with `StopReason::Cancelled`\\n\\nSee protocol docs: [Cancellation](https://agentclientprotocol.com/protocol/prompt-turn#cancellation)\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/CancelNotification\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"ExtNotification\","]
#[doc = "      \"description\": \"Handles extension notifications from the client.\\n\\nExtension notifications provide a way to send one-way messages for custom functionality\\nwhile maintaining protocol compatibility.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/ExtNotification\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct ClientNotificationParams {
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_0: ::std::option::Option<CancelNotification>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_1: ::std::option::Option<ExtNotification>,
}
impl ::std::default::Default for ClientNotificationParams {
    fn default() -> Self {
        Self {
            subtype_0: Default::default(),
            subtype_1: Default::default(),
        }
    }
}
impl ClientNotificationParams {
    pub fn builder() -> builder::ClientNotificationParams {
        Default::default()
    }
}
#[doc = "A JSON-RPC request object."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"A JSON-RPC request object.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"id\","]
#[doc = "    \"method\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"id\": {"]
#[doc = "      \"description\": \"The request id used to correlate the matching response.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/RequestId\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    \"method\": {"]
#[doc = "      \"description\": \"The method name to invoke.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"params\": {"]
#[doc = "      \"description\": \"Method-specific request parameters.\","]
#[doc = "      \"anyOf\": ["]
#[doc = "        {"]
#[doc = "          \"description\": \"All possible requests that a client can send to an agent.\\n\\nThis enum is used internally for routing RPC requests. You typically won't need\\nto use this directly.\\n\\nThis enum encompasses all method calls from client to agent.\","]
#[doc = "          \"anyOf\": ["]
#[doc = "            {"]
#[doc = "              \"title\": \"InitializeRequest\","]
#[doc = "              \"description\": \"Establishes the connection with a client and negotiates protocol capabilities.\\n\\nThis method is called once at the beginning of the connection to:\\n- Negotiate the protocol version to use\\n- Exchange capability information between client and agent\\n- Determine available authentication methods\\n\\nThe agent should respond with its supported protocol version and capabilities.\\n\\nSee protocol docs: [Initialization](https://agentclientprotocol.com/protocol/initialization)\","]
#[doc = "              \"allOf\": ["]
#[doc = "                {"]
#[doc = "                  \"$ref\": \"#/$defs/InitializeRequest\""]
#[doc = "                }"]
#[doc = "              ]"]
#[doc = "            },"]
#[doc = "            {"]
#[doc = "              \"title\": \"AuthenticateRequest\","]
#[doc = "              \"description\": \"Authenticates the client using the specified authentication method.\\n\\nCalled when the agent requires authentication before allowing session creation.\\nThe client provides the authentication method ID that was advertised during initialization.\\n\\nAfter successful authentication, the client can proceed to create sessions with\\n`new_session` without receiving an `auth_required` error.\\n\\nSee protocol docs: [Initialization](https://agentclientprotocol.com/protocol/initialization)\","]
#[doc = "              \"allOf\": ["]
#[doc = "                {"]
#[doc = "                  \"$ref\": \"#/$defs/AuthenticateRequest\""]
#[doc = "                }"]
#[doc = "              ]"]
#[doc = "            },"]
#[doc = "            {"]
#[doc = "              \"title\": \"LogoutRequest\","]
#[doc = "              \"description\": \"Logs out of the current authenticated state.\\n\\nAfter a successful logout, all new sessions will require authentication.\\nThere is no guarantee about the behavior of already running sessions.\","]
#[doc = "              \"allOf\": ["]
#[doc = "                {"]
#[doc = "                  \"$ref\": \"#/$defs/LogoutRequest\""]
#[doc = "                }"]
#[doc = "              ]"]
#[doc = "            },"]
#[doc = "            {"]
#[doc = "              \"title\": \"NewSessionRequest\","]
#[doc = "              \"description\": \"Creates a new conversation session with the agent.\\n\\nSessions represent independent conversation contexts with their own history and state.\\n\\nThe agent should:\\n- Create a new session context\\n- Connect to any specified MCP servers\\n- Return a unique session ID for future requests\\n\\nMay return an `auth_required` error if the agent requires authentication.\\n\\nSee protocol docs: [Session Setup](https://agentclientprotocol.com/protocol/session-setup)\","]
#[doc = "              \"allOf\": ["]
#[doc = "                {"]
#[doc = "                  \"$ref\": \"#/$defs/NewSessionRequest\""]
#[doc = "                }"]
#[doc = "              ]"]
#[doc = "            },"]
#[doc = "            {"]
#[doc = "              \"title\": \"LoadSessionRequest\","]
#[doc = "              \"description\": \"Loads an existing session to resume a previous conversation.\\n\\nThis method is only available if the agent advertises the `loadSession` capability.\\n\\nThe agent should:\\n- Restore the session context and conversation history\\n- Connect to the specified MCP servers\\n- Stream the entire conversation history back to the client via notifications\\n\\nSee protocol docs: [Loading Sessions](https://agentclientprotocol.com/protocol/session-setup#loading-sessions)\","]
#[doc = "              \"allOf\": ["]
#[doc = "                {"]
#[doc = "                  \"$ref\": \"#/$defs/LoadSessionRequest\""]
#[doc = "                }"]
#[doc = "              ]"]
#[doc = "            },"]
#[doc = "            {"]
#[doc = "              \"title\": \"ListSessionsRequest\","]
#[doc = "              \"description\": \"Lists existing sessions known to the agent.\\n\\nThis method is only available if the agent advertises the `sessionCapabilities.list` capability.\\n\\nThe agent should return metadata about sessions with optional filtering and pagination support.\","]
#[doc = "              \"allOf\": ["]
#[doc = "                {"]
#[doc = "                  \"$ref\": \"#/$defs/ListSessionsRequest\""]
#[doc = "                }"]
#[doc = "              ]"]
#[doc = "            },"]
#[doc = "            {"]
#[doc = "              \"title\": \"DeleteSessionRequest\","]
#[doc = "              \"description\": \"Deletes an existing session from `session/list`.\\n\\nThis method is only available if the agent advertises the `sessionCapabilities.delete` capability.\","]
#[doc = "              \"allOf\": ["]
#[doc = "                {"]
#[doc = "                  \"$ref\": \"#/$defs/DeleteSessionRequest\""]
#[doc = "                }"]
#[doc = "              ]"]
#[doc = "            },"]
#[doc = "            {"]
#[doc = "              \"title\": \"ResumeSessionRequest\","]
#[doc = "              \"description\": \"Resumes an existing session without returning previous messages.\\n\\nThis method is only available if the agent advertises the `sessionCapabilities.resume` capability.\\n\\nThe agent should resume the session context, allowing the conversation to continue\\nwithout replaying the message history (unlike `session/load`).\","]
#[doc = "              \"allOf\": ["]
#[doc = "                {"]
#[doc = "                  \"$ref\": \"#/$defs/ResumeSessionRequest\""]
#[doc = "                }"]
#[doc = "              ]"]
#[doc = "            },"]
#[doc = "            {"]
#[doc = "              \"title\": \"CloseSessionRequest\","]
#[doc = "              \"description\": \"Closes an active session and frees up any resources associated with it.\\n\\nThis method is only available if the agent advertises the `sessionCapabilities.close` capability.\\n\\nThe agent must cancel any ongoing work (as if `session/cancel` was called)\\nand then free up any resources associated with the session.\","]
#[doc = "              \"allOf\": ["]
#[doc = "                {"]
#[doc = "                  \"$ref\": \"#/$defs/CloseSessionRequest\""]
#[doc = "                }"]
#[doc = "              ]"]
#[doc = "            },"]
#[doc = "            {"]
#[doc = "              \"title\": \"SetSessionModeRequest\","]
#[doc = "              \"description\": \"Sets the current mode for a session.\\n\\nAllows switching between different agent modes (e.g., \\\"ask\\\", \\\"architect\\\", \\\"code\\\")\\nthat affect system prompts, tool availability, and permission behaviors.\\n\\nThe mode must be one of the modes advertised in `availableModes` during session\\ncreation or loading. Agents may also change modes autonomously and notify the\\nclient via `current_mode_update` notifications.\\n\\nThis method can be called at any time during a session, whether the Agent is\\nidle or actively generating a response.\\n\\nSee protocol docs: [Session Modes](https://agentclientprotocol.com/protocol/session-modes)\","]
#[doc = "              \"allOf\": ["]
#[doc = "                {"]
#[doc = "                  \"$ref\": \"#/$defs/SetSessionModeRequest\""]
#[doc = "                }"]
#[doc = "              ]"]
#[doc = "            },"]
#[doc = "            {"]
#[doc = "              \"title\": \"SetSessionConfigOptionRequest\","]
#[doc = "              \"description\": \"Sets the current value for a session configuration option.\","]
#[doc = "              \"allOf\": ["]
#[doc = "                {"]
#[doc = "                  \"$ref\": \"#/$defs/SetSessionConfigOptionRequest\""]
#[doc = "                }"]
#[doc = "              ]"]
#[doc = "            },"]
#[doc = "            {"]
#[doc = "              \"title\": \"PromptRequest\","]
#[doc = "              \"description\": \"Processes a user prompt within a session.\\n\\nThis method handles the whole lifecycle of a prompt:\\n- Receives user messages with optional context (files, images, etc.)\\n- Processes the prompt using language models\\n- Reports language model content and tool calls to the Clients\\n- Requests permission to run tools\\n- Executes any requested tool calls\\n- Returns when the turn is complete with a stop reason\\n\\nSee protocol docs: [Prompt Turn](https://agentclientprotocol.com/protocol/prompt-turn)\","]
#[doc = "              \"allOf\": ["]
#[doc = "                {"]
#[doc = "                  \"$ref\": \"#/$defs/PromptRequest\""]
#[doc = "                }"]
#[doc = "              ]"]
#[doc = "            },"]
#[doc = "            {"]
#[doc = "              \"title\": \"ExtMethodRequest\","]
#[doc = "              \"description\": \"Handles extension method requests from the client.\\n\\nExtension methods provide a way to add custom functionality while maintaining\\nprotocol compatibility.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "              \"allOf\": ["]
#[doc = "                {"]
#[doc = "                  \"$ref\": \"#/$defs/ExtRequest\""]
#[doc = "                }"]
#[doc = "              ]"]
#[doc = "            }"]
#[doc = "          ]"]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"type\": \"null\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"x-docs-ignore\": true"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct ClientRequest {
    #[doc = "The request id used to correlate the matching response."]
    pub id: RequestId,
    #[doc = "The method name to invoke."]
    pub method: ::std::string::String,
    #[doc = "Method-specific request parameters."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub params: ::std::option::Option<ClientRequestParams>,
}
impl ClientRequest {
    pub fn builder() -> builder::ClientRequest {
        Default::default()
    }
}
#[doc = "All possible requests that a client can send to an agent.\n\nThis enum is used internally for routing RPC requests. You typically won't need\nto use this directly.\n\nThis enum encompasses all method calls from client to agent."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"All possible requests that a client can send to an agent.\\n\\nThis enum is used internally for routing RPC requests. You typically won't need\\nto use this directly.\\n\\nThis enum encompasses all method calls from client to agent.\","]
#[doc = "  \"anyOf\": ["]
#[doc = "    {"]
#[doc = "      \"title\": \"InitializeRequest\","]
#[doc = "      \"description\": \"Establishes the connection with a client and negotiates protocol capabilities.\\n\\nThis method is called once at the beginning of the connection to:\\n- Negotiate the protocol version to use\\n- Exchange capability information between client and agent\\n- Determine available authentication methods\\n\\nThe agent should respond with its supported protocol version and capabilities.\\n\\nSee protocol docs: [Initialization](https://agentclientprotocol.com/protocol/initialization)\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/InitializeRequest\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"AuthenticateRequest\","]
#[doc = "      \"description\": \"Authenticates the client using the specified authentication method.\\n\\nCalled when the agent requires authentication before allowing session creation.\\nThe client provides the authentication method ID that was advertised during initialization.\\n\\nAfter successful authentication, the client can proceed to create sessions with\\n`new_session` without receiving an `auth_required` error.\\n\\nSee protocol docs: [Initialization](https://agentclientprotocol.com/protocol/initialization)\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/AuthenticateRequest\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"LogoutRequest\","]
#[doc = "      \"description\": \"Logs out of the current authenticated state.\\n\\nAfter a successful logout, all new sessions will require authentication.\\nThere is no guarantee about the behavior of already running sessions.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/LogoutRequest\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"NewSessionRequest\","]
#[doc = "      \"description\": \"Creates a new conversation session with the agent.\\n\\nSessions represent independent conversation contexts with their own history and state.\\n\\nThe agent should:\\n- Create a new session context\\n- Connect to any specified MCP servers\\n- Return a unique session ID for future requests\\n\\nMay return an `auth_required` error if the agent requires authentication.\\n\\nSee protocol docs: [Session Setup](https://agentclientprotocol.com/protocol/session-setup)\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/NewSessionRequest\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"LoadSessionRequest\","]
#[doc = "      \"description\": \"Loads an existing session to resume a previous conversation.\\n\\nThis method is only available if the agent advertises the `loadSession` capability.\\n\\nThe agent should:\\n- Restore the session context and conversation history\\n- Connect to the specified MCP servers\\n- Stream the entire conversation history back to the client via notifications\\n\\nSee protocol docs: [Loading Sessions](https://agentclientprotocol.com/protocol/session-setup#loading-sessions)\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/LoadSessionRequest\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"ListSessionsRequest\","]
#[doc = "      \"description\": \"Lists existing sessions known to the agent.\\n\\nThis method is only available if the agent advertises the `sessionCapabilities.list` capability.\\n\\nThe agent should return metadata about sessions with optional filtering and pagination support.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/ListSessionsRequest\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"DeleteSessionRequest\","]
#[doc = "      \"description\": \"Deletes an existing session from `session/list`.\\n\\nThis method is only available if the agent advertises the `sessionCapabilities.delete` capability.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/DeleteSessionRequest\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"ResumeSessionRequest\","]
#[doc = "      \"description\": \"Resumes an existing session without returning previous messages.\\n\\nThis method is only available if the agent advertises the `sessionCapabilities.resume` capability.\\n\\nThe agent should resume the session context, allowing the conversation to continue\\nwithout replaying the message history (unlike `session/load`).\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/ResumeSessionRequest\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"CloseSessionRequest\","]
#[doc = "      \"description\": \"Closes an active session and frees up any resources associated with it.\\n\\nThis method is only available if the agent advertises the `sessionCapabilities.close` capability.\\n\\nThe agent must cancel any ongoing work (as if `session/cancel` was called)\\nand then free up any resources associated with the session.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/CloseSessionRequest\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"SetSessionModeRequest\","]
#[doc = "      \"description\": \"Sets the current mode for a session.\\n\\nAllows switching between different agent modes (e.g., \\\"ask\\\", \\\"architect\\\", \\\"code\\\")\\nthat affect system prompts, tool availability, and permission behaviors.\\n\\nThe mode must be one of the modes advertised in `availableModes` during session\\ncreation or loading. Agents may also change modes autonomously and notify the\\nclient via `current_mode_update` notifications.\\n\\nThis method can be called at any time during a session, whether the Agent is\\nidle or actively generating a response.\\n\\nSee protocol docs: [Session Modes](https://agentclientprotocol.com/protocol/session-modes)\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/SetSessionModeRequest\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"SetSessionConfigOptionRequest\","]
#[doc = "      \"description\": \"Sets the current value for a session configuration option.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/SetSessionConfigOptionRequest\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"PromptRequest\","]
#[doc = "      \"description\": \"Processes a user prompt within a session.\\n\\nThis method handles the whole lifecycle of a prompt:\\n- Receives user messages with optional context (files, images, etc.)\\n- Processes the prompt using language models\\n- Reports language model content and tool calls to the Clients\\n- Requests permission to run tools\\n- Executes any requested tool calls\\n- Returns when the turn is complete with a stop reason\\n\\nSee protocol docs: [Prompt Turn](https://agentclientprotocol.com/protocol/prompt-turn)\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/PromptRequest\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"ExtMethodRequest\","]
#[doc = "      \"description\": \"Handles extension method requests from the client.\\n\\nExtension methods provide a way to add custom functionality while maintaining\\nprotocol compatibility.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/ExtRequest\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct ClientRequestParams {
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_0: ::std::option::Option<InitializeRequest>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_1: ::std::option::Option<AuthenticateRequest>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_2: ::std::option::Option<LogoutRequest>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_3: ::std::option::Option<NewSessionRequest>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_4: ::std::option::Option<LoadSessionRequest>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_5: ::std::option::Option<ListSessionsRequest>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_6: ::std::option::Option<DeleteSessionRequest>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_7: ::std::option::Option<ResumeSessionRequest>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_8: ::std::option::Option<CloseSessionRequest>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_9: ::std::option::Option<SetSessionModeRequest>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_10: ::std::option::Option<SetSessionConfigOptionRequest>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_11: ::std::option::Option<PromptRequest>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_12: ::std::option::Option<ExtRequest>,
}
impl ::std::default::Default for ClientRequestParams {
    fn default() -> Self {
        Self {
            subtype_0: Default::default(),
            subtype_1: Default::default(),
            subtype_2: Default::default(),
            subtype_3: Default::default(),
            subtype_4: Default::default(),
            subtype_5: Default::default(),
            subtype_6: Default::default(),
            subtype_7: Default::default(),
            subtype_8: Default::default(),
            subtype_9: Default::default(),
            subtype_10: Default::default(),
            subtype_11: Default::default(),
            subtype_12: Default::default(),
        }
    }
}
impl ClientRequestParams {
    pub fn builder() -> builder::ClientRequestParams {
        Default::default()
    }
}
#[doc = "A JSON-RPC response object."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"A JSON-RPC response object.\","]
#[doc = "  \"anyOf\": ["]
#[doc = "    {"]
#[doc = "      \"title\": \"Result\","]
#[doc = "      \"description\": \"A successful JSON-RPC response.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"required\": ["]
#[doc = "        \"id\","]
#[doc = "        \"result\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"id\": {"]
#[doc = "          \"description\": \"The id of the request this response answers.\","]
#[doc = "          \"allOf\": ["]
#[doc = "            {"]
#[doc = "              \"$ref\": \"#/$defs/RequestId\""]
#[doc = "            }"]
#[doc = "          ]"]
#[doc = "        },"]
#[doc = "        \"result\": {"]
#[doc = "          \"description\": \"Method-specific response data.\","]
#[doc = "          \"anyOf\": ["]
#[doc = "            {"]
#[doc = "              \"title\": \"WriteTextFileResponse\","]
#[doc = "              \"description\": \"Successful result returned for a `fs/write_text_file` request.\","]
#[doc = "              \"allOf\": ["]
#[doc = "                {"]
#[doc = "                  \"$ref\": \"#/$defs/WriteTextFileResponse\""]
#[doc = "                }"]
#[doc = "              ]"]
#[doc = "            },"]
#[doc = "            {"]
#[doc = "              \"title\": \"ReadTextFileResponse\","]
#[doc = "              \"description\": \"Successful result returned for a `fs/read_text_file` request.\","]
#[doc = "              \"allOf\": ["]
#[doc = "                {"]
#[doc = "                  \"$ref\": \"#/$defs/ReadTextFileResponse\""]
#[doc = "                }"]
#[doc = "              ]"]
#[doc = "            },"]
#[doc = "            {"]
#[doc = "              \"title\": \"RequestPermissionResponse\","]
#[doc = "              \"description\": \"Successful result returned for a `session/request_permission` request.\","]
#[doc = "              \"allOf\": ["]
#[doc = "                {"]
#[doc = "                  \"$ref\": \"#/$defs/RequestPermissionResponse\""]
#[doc = "                }"]
#[doc = "              ]"]
#[doc = "            },"]
#[doc = "            {"]
#[doc = "              \"title\": \"CreateTerminalResponse\","]
#[doc = "              \"description\": \"Successful result returned for a `terminal/create` request.\","]
#[doc = "              \"allOf\": ["]
#[doc = "                {"]
#[doc = "                  \"$ref\": \"#/$defs/CreateTerminalResponse\""]
#[doc = "                }"]
#[doc = "              ]"]
#[doc = "            },"]
#[doc = "            {"]
#[doc = "              \"title\": \"TerminalOutputResponse\","]
#[doc = "              \"description\": \"Successful result returned for a `terminal/output` request.\","]
#[doc = "              \"allOf\": ["]
#[doc = "                {"]
#[doc = "                  \"$ref\": \"#/$defs/TerminalOutputResponse\""]
#[doc = "                }"]
#[doc = "              ]"]
#[doc = "            },"]
#[doc = "            {"]
#[doc = "              \"title\": \"ReleaseTerminalResponse\","]
#[doc = "              \"description\": \"Successful result returned for a `terminal/release` request.\","]
#[doc = "              \"allOf\": ["]
#[doc = "                {"]
#[doc = "                  \"$ref\": \"#/$defs/ReleaseTerminalResponse\""]
#[doc = "                }"]
#[doc = "              ]"]
#[doc = "            },"]
#[doc = "            {"]
#[doc = "              \"title\": \"WaitForTerminalExitResponse\","]
#[doc = "              \"description\": \"Successful result returned for a `terminal/wait_for_exit` request.\","]
#[doc = "              \"allOf\": ["]
#[doc = "                {"]
#[doc = "                  \"$ref\": \"#/$defs/WaitForTerminalExitResponse\""]
#[doc = "                }"]
#[doc = "              ]"]
#[doc = "            },"]
#[doc = "            {"]
#[doc = "              \"title\": \"KillTerminalResponse\","]
#[doc = "              \"description\": \"Successful result returned for a `terminal/kill` request.\","]
#[doc = "              \"allOf\": ["]
#[doc = "                {"]
#[doc = "                  \"$ref\": \"#/$defs/KillTerminalResponse\""]
#[doc = "                }"]
#[doc = "              ]"]
#[doc = "            },"]
#[doc = "            {"]
#[doc = "              \"title\": \"ExtMethodResponse\","]
#[doc = "              \"description\": \"Successful result returned by an extension method outside the core ACP method set.\","]
#[doc = "              \"allOf\": ["]
#[doc = "                {"]
#[doc = "                  \"$ref\": \"#/$defs/ExtResponse\""]
#[doc = "                }"]
#[doc = "              ]"]
#[doc = "            }"]
#[doc = "          ]"]
#[doc = "        }"]
#[doc = "      }"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"Error\","]
#[doc = "      \"description\": \"A failed JSON-RPC response.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"required\": ["]
#[doc = "        \"error\","]
#[doc = "        \"id\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"error\": {"]
#[doc = "          \"description\": \"Method-specific error data.\","]
#[doc = "          \"allOf\": ["]
#[doc = "            {"]
#[doc = "              \"$ref\": \"#/$defs/Error\""]
#[doc = "            }"]
#[doc = "          ]"]
#[doc = "        },"]
#[doc = "        \"id\": {"]
#[doc = "          \"description\": \"The id of the request this response answers.\","]
#[doc = "          \"allOf\": ["]
#[doc = "            {"]
#[doc = "              \"$ref\": \"#/$defs/RequestId\""]
#[doc = "            }"]
#[doc = "          ]"]
#[doc = "        }"]
#[doc = "      }"]
#[doc = "    }"]
#[doc = "  ],"]
#[doc = "  \"x-docs-ignore\": true"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
#[serde(untagged)]
pub enum ClientResponse {
    Result {
        #[doc = "The id of the request this response answers."]
        id: RequestId,
        #[doc = "Method-specific response data."]
        result: ResultResult,
    },
    Error {
        #[doc = "Method-specific error data."]
        error: Error,
        #[doc = "The id of the request this response answers."]
        id: RequestId,
    },
}
#[doc = "Session-related capabilities supported by the client."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Session-related capabilities supported by the client.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"configOptions\": {"]
#[doc = "      \"description\": \"Config option capabilities supported by the client.\\n\\nOmitted or `null` both mean the client does not advertise support for any\\nconfig option extensions.\","]
#[doc = "      \"anyOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/SessionConfigOptionsCapabilities\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"type\": \"null\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct ClientSessionCapabilities {
    #[doc = "Config option capabilities supported by the client.\n\nOmitted or `null` both mean the client does not advertise support for any\nconfig option extensions."]
    #[serde(
        rename = "configOptions",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub config_options: ::std::option::Option<SessionConfigOptionsCapabilities>,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
}
impl ::std::default::Default for ClientSessionCapabilities {
    fn default() -> Self {
        Self {
            config_options: Default::default(),
            meta: Default::default(),
        }
    }
}
impl ClientSessionCapabilities {
    pub fn builder() -> builder::ClientSessionCapabilities {
        Default::default()
    }
}
#[doc = "`ClientVariant0Jsonrpc`"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"type\": \"string\","]
#[doc = "  \"enum\": ["]
#[doc = "    \"2.0\""]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(
    :: serde :: Deserialize,
    :: serde :: Serialize,
    Clone,
    Copy,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
)]
pub enum ClientVariant0Jsonrpc {
    #[serde(rename = "2.0")]
    X20,
}
impl ::std::fmt::Display for ClientVariant0Jsonrpc {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        match *self {
            Self::X20 => f.write_str("2.0"),
        }
    }
}
impl ::std::str::FromStr for ClientVariant0Jsonrpc {
    type Err = self::error::ConversionError;
    fn from_str(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        match value {
            "2.0" => Ok(Self::X20),
            _ => Err("invalid value".into()),
        }
    }
}
impl ::std::convert::TryFrom<&str> for ClientVariant0Jsonrpc {
    type Error = self::error::ConversionError;
    fn try_from(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<&::std::string::String> for ClientVariant0Jsonrpc {
    type Error = self::error::ConversionError;
    fn try_from(
        value: &::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<::std::string::String> for ClientVariant0Jsonrpc {
    type Error = self::error::ConversionError;
    fn try_from(
        value: ::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
#[doc = "All possible requests that a client can send to an agent.\n\nThis enum is used internally for routing RPC requests. You typically won't need\nto use this directly.\n\nThis enum encompasses all method calls from client to agent."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"All possible requests that a client can send to an agent.\\n\\nThis enum is used internally for routing RPC requests. You typically won't need\\nto use this directly.\\n\\nThis enum encompasses all method calls from client to agent.\","]
#[doc = "  \"anyOf\": ["]
#[doc = "    {"]
#[doc = "      \"title\": \"InitializeRequest\","]
#[doc = "      \"description\": \"Establishes the connection with a client and negotiates protocol capabilities.\\n\\nThis method is called once at the beginning of the connection to:\\n- Negotiate the protocol version to use\\n- Exchange capability information between client and agent\\n- Determine available authentication methods\\n\\nThe agent should respond with its supported protocol version and capabilities.\\n\\nSee protocol docs: [Initialization](https://agentclientprotocol.com/protocol/initialization)\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/InitializeRequest\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"AuthenticateRequest\","]
#[doc = "      \"description\": \"Authenticates the client using the specified authentication method.\\n\\nCalled when the agent requires authentication before allowing session creation.\\nThe client provides the authentication method ID that was advertised during initialization.\\n\\nAfter successful authentication, the client can proceed to create sessions with\\n`new_session` without receiving an `auth_required` error.\\n\\nSee protocol docs: [Initialization](https://agentclientprotocol.com/protocol/initialization)\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/AuthenticateRequest\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"LogoutRequest\","]
#[doc = "      \"description\": \"Logs out of the current authenticated state.\\n\\nAfter a successful logout, all new sessions will require authentication.\\nThere is no guarantee about the behavior of already running sessions.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/LogoutRequest\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"NewSessionRequest\","]
#[doc = "      \"description\": \"Creates a new conversation session with the agent.\\n\\nSessions represent independent conversation contexts with their own history and state.\\n\\nThe agent should:\\n- Create a new session context\\n- Connect to any specified MCP servers\\n- Return a unique session ID for future requests\\n\\nMay return an `auth_required` error if the agent requires authentication.\\n\\nSee protocol docs: [Session Setup](https://agentclientprotocol.com/protocol/session-setup)\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/NewSessionRequest\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"LoadSessionRequest\","]
#[doc = "      \"description\": \"Loads an existing session to resume a previous conversation.\\n\\nThis method is only available if the agent advertises the `loadSession` capability.\\n\\nThe agent should:\\n- Restore the session context and conversation history\\n- Connect to the specified MCP servers\\n- Stream the entire conversation history back to the client via notifications\\n\\nSee protocol docs: [Loading Sessions](https://agentclientprotocol.com/protocol/session-setup#loading-sessions)\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/LoadSessionRequest\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"ListSessionsRequest\","]
#[doc = "      \"description\": \"Lists existing sessions known to the agent.\\n\\nThis method is only available if the agent advertises the `sessionCapabilities.list` capability.\\n\\nThe agent should return metadata about sessions with optional filtering and pagination support.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/ListSessionsRequest\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"DeleteSessionRequest\","]
#[doc = "      \"description\": \"Deletes an existing session from `session/list`.\\n\\nThis method is only available if the agent advertises the `sessionCapabilities.delete` capability.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/DeleteSessionRequest\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"ResumeSessionRequest\","]
#[doc = "      \"description\": \"Resumes an existing session without returning previous messages.\\n\\nThis method is only available if the agent advertises the `sessionCapabilities.resume` capability.\\n\\nThe agent should resume the session context, allowing the conversation to continue\\nwithout replaying the message history (unlike `session/load`).\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/ResumeSessionRequest\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"CloseSessionRequest\","]
#[doc = "      \"description\": \"Closes an active session and frees up any resources associated with it.\\n\\nThis method is only available if the agent advertises the `sessionCapabilities.close` capability.\\n\\nThe agent must cancel any ongoing work (as if `session/cancel` was called)\\nand then free up any resources associated with the session.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/CloseSessionRequest\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"SetSessionModeRequest\","]
#[doc = "      \"description\": \"Sets the current mode for a session.\\n\\nAllows switching between different agent modes (e.g., \\\"ask\\\", \\\"architect\\\", \\\"code\\\")\\nthat affect system prompts, tool availability, and permission behaviors.\\n\\nThe mode must be one of the modes advertised in `availableModes` during session\\ncreation or loading. Agents may also change modes autonomously and notify the\\nclient via `current_mode_update` notifications.\\n\\nThis method can be called at any time during a session, whether the Agent is\\nidle or actively generating a response.\\n\\nSee protocol docs: [Session Modes](https://agentclientprotocol.com/protocol/session-modes)\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/SetSessionModeRequest\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"SetSessionConfigOptionRequest\","]
#[doc = "      \"description\": \"Sets the current value for a session configuration option.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/SetSessionConfigOptionRequest\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"PromptRequest\","]
#[doc = "      \"description\": \"Processes a user prompt within a session.\\n\\nThis method handles the whole lifecycle of a prompt:\\n- Receives user messages with optional context (files, images, etc.)\\n- Processes the prompt using language models\\n- Reports language model content and tool calls to the Clients\\n- Requests permission to run tools\\n- Executes any requested tool calls\\n- Returns when the turn is complete with a stop reason\\n\\nSee protocol docs: [Prompt Turn](https://agentclientprotocol.com/protocol/prompt-turn)\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/PromptRequest\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"ExtMethodRequest\","]
#[doc = "      \"description\": \"Handles extension method requests from the client.\\n\\nExtension methods provide a way to add custom functionality while maintaining\\nprotocol compatibility.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/ExtRequest\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct ClientVariant0Params {
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_0: ::std::option::Option<InitializeRequest>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_1: ::std::option::Option<AuthenticateRequest>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_2: ::std::option::Option<LogoutRequest>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_3: ::std::option::Option<NewSessionRequest>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_4: ::std::option::Option<LoadSessionRequest>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_5: ::std::option::Option<ListSessionsRequest>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_6: ::std::option::Option<DeleteSessionRequest>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_7: ::std::option::Option<ResumeSessionRequest>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_8: ::std::option::Option<CloseSessionRequest>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_9: ::std::option::Option<SetSessionModeRequest>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_10: ::std::option::Option<SetSessionConfigOptionRequest>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_11: ::std::option::Option<PromptRequest>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_12: ::std::option::Option<ExtRequest>,
}
impl ::std::default::Default for ClientVariant0Params {
    fn default() -> Self {
        Self {
            subtype_0: Default::default(),
            subtype_1: Default::default(),
            subtype_2: Default::default(),
            subtype_3: Default::default(),
            subtype_4: Default::default(),
            subtype_5: Default::default(),
            subtype_6: Default::default(),
            subtype_7: Default::default(),
            subtype_8: Default::default(),
            subtype_9: Default::default(),
            subtype_10: Default::default(),
            subtype_11: Default::default(),
            subtype_12: Default::default(),
        }
    }
}
impl ClientVariant0Params {
    pub fn builder() -> builder::ClientVariant0Params {
        Default::default()
    }
}
#[doc = "`ClientVariant1`"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"allOf\": ["]
#[doc = "    {"]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"required\": ["]
#[doc = "        \"jsonrpc\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"jsonrpc\": {"]
#[doc = "          \"type\": \"string\","]
#[doc = "          \"enum\": ["]
#[doc = "            \"2.0\""]
#[doc = "          ]"]
#[doc = "        }"]
#[doc = "      }"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"Response\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/ClientResponse\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"not\": {"]
#[doc = "        \"title\": \"Request\","]
#[doc = "        \"allOf\": ["]
#[doc = "          {"]
#[doc = "            \"$ref\": \"#/$defs/ClientRequest\""]
#[doc = "          }"]
#[doc = "        ]"]
#[doc = "      }"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"not\": {"]
#[doc = "        \"title\": \"Notification\","]
#[doc = "        \"allOf\": ["]
#[doc = "          {"]
#[doc = "            \"$ref\": \"#/$defs/ClientNotification\""]
#[doc = "          }"]
#[doc = "        ]"]
#[doc = "      }"]
#[doc = "    }"]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(
    :: serde :: Deserialize,
    :: serde :: Serialize,
    Clone,
    Copy,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
)]
#[serde(deny_unknown_fields)]
pub enum ClientVariant1 {}
#[doc = "`ClientVariant2Jsonrpc`"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"type\": \"string\","]
#[doc = "  \"enum\": ["]
#[doc = "    \"2.0\""]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(
    :: serde :: Deserialize,
    :: serde :: Serialize,
    Clone,
    Copy,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
)]
pub enum ClientVariant2Jsonrpc {
    #[serde(rename = "2.0")]
    X20,
}
impl ::std::fmt::Display for ClientVariant2Jsonrpc {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        match *self {
            Self::X20 => f.write_str("2.0"),
        }
    }
}
impl ::std::str::FromStr for ClientVariant2Jsonrpc {
    type Err = self::error::ConversionError;
    fn from_str(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        match value {
            "2.0" => Ok(Self::X20),
            _ => Err("invalid value".into()),
        }
    }
}
impl ::std::convert::TryFrom<&str> for ClientVariant2Jsonrpc {
    type Error = self::error::ConversionError;
    fn try_from(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<&::std::string::String> for ClientVariant2Jsonrpc {
    type Error = self::error::ConversionError;
    fn try_from(
        value: &::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<::std::string::String> for ClientVariant2Jsonrpc {
    type Error = self::error::ConversionError;
    fn try_from(
        value: ::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
#[doc = "All possible notifications that a client can send to an agent.\n\nThis enum is used internally for routing RPC notifications. You typically won't need\nto use this directly.\n\nNotifications do not expect a response."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"All possible notifications that a client can send to an agent.\\n\\nThis enum is used internally for routing RPC notifications. You typically won't need\\nto use this directly.\\n\\nNotifications do not expect a response.\","]
#[doc = "  \"anyOf\": ["]
#[doc = "    {"]
#[doc = "      \"title\": \"CancelNotification\","]
#[doc = "      \"description\": \"Cancels ongoing operations for a session.\\n\\nThis is a notification sent by the client to cancel an ongoing prompt turn.\\n\\nUpon receiving this notification, the Agent SHOULD:\\n- Stop all language model requests as soon as possible\\n- Abort all tool call invocations in progress\\n- Send any pending `session/update` notifications\\n- Respond to the original `session/prompt` request with `StopReason::Cancelled`\\n\\nSee protocol docs: [Cancellation](https://agentclientprotocol.com/protocol/prompt-turn#cancellation)\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/CancelNotification\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"ExtNotification\","]
#[doc = "      \"description\": \"Handles extension notifications from the client.\\n\\nExtension notifications provide a way to send one-way messages for custom functionality\\nwhile maintaining protocol compatibility.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/ExtNotification\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct ClientVariant2Params {
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_0: ::std::option::Option<CancelNotification>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_1: ::std::option::Option<ExtNotification>,
}
impl ::std::default::Default for ClientVariant2Params {
    fn default() -> Self {
        Self {
            subtype_0: Default::default(),
            subtype_1: Default::default(),
        }
    }
}
impl ClientVariant2Params {
    pub fn builder() -> builder::ClientVariant2Params {
        Default::default()
    }
}
#[doc = "Request parameters for closing an active session.\n\nIf supported, the agent **must** cancel any ongoing work related to the session\n(treat it as if `session/cancel` was called) and then free up any resources\nassociated with the session.\n\nOnly available if the Agent supports the `sessionCapabilities.close` capability."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Request parameters for closing an active session.\\n\\nIf supported, the agent **must** cancel any ongoing work related to the session\\n(treat it as if `session/cancel` was called) and then free up any resources\\nassociated with the session.\\n\\nOnly available if the Agent supports the `sessionCapabilities.close` capability.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"sessionId\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"sessionId\": {"]
#[doc = "      \"description\": \"The ID of the session to close.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/SessionId\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"x-method\": \"session/close\","]
#[doc = "  \"x-side\": \"agent\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct CloseSessionRequest {
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "The ID of the session to close."]
    #[serde(rename = "sessionId")]
    pub session_id: SessionId,
}
impl CloseSessionRequest {
    pub fn builder() -> builder::CloseSessionRequest {
        Default::default()
    }
}
#[doc = "Response from closing a session."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Response from closing a session.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"x-method\": \"session/close\","]
#[doc = "  \"x-side\": \"agent\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct CloseSessionResponse {
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
}
impl ::std::default::Default for CloseSessionResponse {
    fn default() -> Self {
        Self {
            meta: Default::default(),
        }
    }
}
impl CloseSessionResponse {
    pub fn builder() -> builder::CloseSessionResponse {
        Default::default()
    }
}
#[doc = "Session configuration options have been updated."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Session configuration options have been updated.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"configOptions\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"configOptions\": {"]
#[doc = "      \"description\": \"The full set of configuration options and their current values.\","]
#[doc = "      \"type\": \"array\","]
#[doc = "      \"items\": {"]
#[doc = "        \"$ref\": \"#/$defs/SessionConfigOption\""]
#[doc = "      },"]
#[doc = "      \"x-deserialize-default-on-error\": true,"]
#[doc = "      \"x-deserialize-skip-invalid-items\": true"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct ConfigOptionUpdate {
    #[doc = "The full set of configuration options and their current values."]
    #[serde(rename = "configOptions")]
    pub config_options: ::std::vec::Vec<SessionConfigOption>,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
}
impl ConfigOptionUpdate {
    pub fn builder() -> builder::ConfigOptionUpdate {
        Default::default()
    }
}
#[doc = "Standard content block (text, images, resources)."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Standard content block (text, images, resources).\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"content\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"content\": {"]
#[doc = "      \"description\": \"The actual content block.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/ContentBlock\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct Content {
    #[doc = "The actual content block."]
    pub content: ContentBlock,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
}
impl Content {
    pub fn builder() -> builder::Content {
        Default::default()
    }
}
#[doc = "Content blocks represent displayable information in the Agent Client Protocol.\n\nThey provide a structured way to handle various types of user-facing content—whether\nit's text from language models, images for analysis, or embedded resources for context.\n\nContent blocks appear in:\n- User prompts sent via `session/prompt`\n- Language model output streamed through `session/update` notifications\n- Progress updates and results from tool calls\n\nThis structure is compatible with the Model Context Protocol (MCP), enabling\nagents to seamlessly forward content from MCP tool outputs without transformation.\n\nSee protocol docs: [Content](https://agentclientprotocol.com/protocol/content)"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Content blocks represent displayable information in the Agent Client Protocol.\\n\\nThey provide a structured way to handle various types of user-facing content—whether\\nit's text from language models, images for analysis, or embedded resources for context.\\n\\nContent blocks appear in:\\n- User prompts sent via `session/prompt`\\n- Language model output streamed through `session/update` notifications\\n- Progress updates and results from tool calls\\n\\nThis structure is compatible with the Model Context Protocol (MCP), enabling\\nagents to seamlessly forward content from MCP tool outputs without transformation.\\n\\nSee protocol docs: [Content](https://agentclientprotocol.com/protocol/content)\","]
#[doc = "  \"oneOf\": ["]
#[doc = "    {"]
#[doc = "      \"description\": \"Text content. May be plain text or formatted with Markdown.\\n\\nAll agents MUST support text content blocks in prompts.\\nClients SHOULD render this text as Markdown.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/TextContent\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"required\": ["]
#[doc = "        \"type\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"type\": {"]
#[doc = "          \"type\": \"string\","]
#[doc = "          \"const\": \"text\""]
#[doc = "        }"]
#[doc = "      }"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"description\": \"Images for visual context or analysis.\\n\\nRequires the `image` prompt capability when included in prompts.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/ImageContent\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"required\": ["]
#[doc = "        \"type\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"type\": {"]
#[doc = "          \"type\": \"string\","]
#[doc = "          \"const\": \"image\""]
#[doc = "        }"]
#[doc = "      }"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"description\": \"Audio data for transcription or analysis.\\n\\nRequires the `audio` prompt capability when included in prompts.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/AudioContent\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"required\": ["]
#[doc = "        \"type\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"type\": {"]
#[doc = "          \"type\": \"string\","]
#[doc = "          \"const\": \"audio\""]
#[doc = "        }"]
#[doc = "      }"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"description\": \"References to resources that the agent can access.\\n\\nAll agents MUST support resource links in prompts.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/ResourceLink\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"required\": ["]
#[doc = "        \"type\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"type\": {"]
#[doc = "          \"type\": \"string\","]
#[doc = "          \"const\": \"resource_link\""]
#[doc = "        }"]
#[doc = "      }"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"description\": \"Complete resource contents embedded directly in the message.\\n\\nPreferred for including context as it avoids extra round-trips.\\n\\nRequires the `embeddedContext` prompt capability when included in prompts.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/EmbeddedResource\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"required\": ["]
#[doc = "        \"type\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"type\": {"]
#[doc = "          \"type\": \"string\","]
#[doc = "          \"const\": \"resource\""]
#[doc = "        }"]
#[doc = "      }"]
#[doc = "    }"]
#[doc = "  ],"]
#[doc = "  \"discriminator\": {"]
#[doc = "    \"propertyName\": \"type\""]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
#[serde(untagged)]
pub enum ContentBlock {
    Variant0 {
        #[doc = "Optional annotations that help clients decide how to display or route this content."]
        #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
        annotations: ::std::option::Option<Annotations>,
        #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
        #[serde(
            rename = "_meta",
            default,
            skip_serializing_if = "::std::option::Option::is_none"
        )]
        meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
        #[doc = "Text payload carried by this content block."]
        text: ::std::string::String,
        #[serde(rename = "type")]
        type_: ::std::string::String,
    },
    Variant1 {
        #[doc = "Optional annotations that help clients decide how to display or route this content."]
        #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
        annotations: ::std::option::Option<Annotations>,
        #[doc = "Base64-encoded media payload."]
        data: ::std::string::String,
        #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
        #[serde(
            rename = "_meta",
            default,
            skip_serializing_if = "::std::option::Option::is_none"
        )]
        meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
        #[doc = "MIME type describing the encoded media payload."]
        #[serde(rename = "mimeType")]
        mime_type: ::std::string::String,
        #[serde(rename = "type")]
        type_: ::std::string::String,
        #[doc = "URI associated with this resource or media payload."]
        #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
        uri: ::std::option::Option<::std::string::String>,
    },
    Variant2 {
        #[doc = "Optional annotations that help clients decide how to display or route this content."]
        #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
        annotations: ::std::option::Option<Annotations>,
        #[doc = "Base64-encoded media payload."]
        data: ::std::string::String,
        #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
        #[serde(
            rename = "_meta",
            default,
            skip_serializing_if = "::std::option::Option::is_none"
        )]
        meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
        #[doc = "MIME type describing the encoded media payload."]
        #[serde(rename = "mimeType")]
        mime_type: ::std::string::String,
        #[serde(rename = "type")]
        type_: ::std::string::String,
    },
    Variant3 {
        #[doc = "Optional annotations that help clients decide how to display or route this content."]
        #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
        annotations: ::std::option::Option<Annotations>,
        #[doc = "Optional human-readable details shown with this protocol object."]
        #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
        description: ::std::option::Option<::std::string::String>,
        #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
        #[serde(
            rename = "_meta",
            default,
            skip_serializing_if = "::std::option::Option::is_none"
        )]
        meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
        #[doc = "MIME type describing the encoded media payload."]
        #[serde(
            rename = "mimeType",
            default,
            skip_serializing_if = "::std::option::Option::is_none"
        )]
        mime_type: ::std::option::Option<::std::string::String>,
        #[doc = "Human-readable name shown for this protocol object."]
        name: ::std::string::String,
        #[doc = "Optional size of the linked resource in bytes, if known."]
        #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
        size: ::std::option::Option<i64>,
        #[doc = "Optional display title for end-user UI."]
        #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
        title: ::std::option::Option<::std::string::String>,
        #[serde(rename = "type")]
        type_: ::std::string::String,
        #[doc = "URI associated with this resource or media payload."]
        uri: ::std::string::String,
    },
    Variant4 {
        #[doc = "Optional annotations that help clients decide how to display or route this content."]
        #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
        annotations: ::std::option::Option<Annotations>,
        #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
        #[serde(
            rename = "_meta",
            default,
            skip_serializing_if = "::std::option::Option::is_none"
        )]
        meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
        #[doc = "Embedded resource payload, either text or binary data."]
        resource: EmbeddedResourceResource,
        #[serde(rename = "type")]
        type_: ::std::string::String,
    },
}
#[doc = "A streamed item of content"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"A streamed item of content\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"content\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"content\": {"]
#[doc = "      \"description\": \"A single item of content\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/ContentBlock\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    \"messageId\": {"]
#[doc = "      \"description\": \"A unique identifier for the message this chunk belongs to.\\n\\nAll chunks belonging to the same message share the same `messageId`.\\nA change in `messageId` indicates a new message has started.\","]
#[doc = "      \"anyOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/MessageId\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"type\": \"null\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct ContentChunk {
    #[doc = "A single item of content"]
    pub content: ContentBlock,
    #[doc = "A unique identifier for the message this chunk belongs to.\n\nAll chunks belonging to the same message share the same `messageId`.\nA change in `messageId` indicates a new message has started."]
    #[serde(
        rename = "messageId",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub message_id: ::std::option::Option<MessageId>,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
}
impl ContentChunk {
    pub fn builder() -> builder::ContentChunk {
        Default::default()
    }
}
#[doc = "Cost information for a session."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Cost information for a session.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"amount\","]
#[doc = "    \"currency\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"amount\": {"]
#[doc = "      \"description\": \"Total cumulative cost for session.\","]
#[doc = "      \"type\": \"number\","]
#[doc = "      \"format\": \"double\""]
#[doc = "    },"]
#[doc = "    \"currency\": {"]
#[doc = "      \"description\": \"ISO 4217 currency code (e.g., \\\"USD\\\", \\\"EUR\\\").\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct Cost {
    #[doc = "Total cumulative cost for session."]
    pub amount: f64,
    #[doc = "ISO 4217 currency code (e.g., \"USD\", \"EUR\")."]
    pub currency: ::std::string::String,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
}
impl Cost {
    pub fn builder() -> builder::Cost {
        Default::default()
    }
}
#[doc = "Request to create a new terminal and execute a command."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Request to create a new terminal and execute a command.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"command\","]
#[doc = "    \"sessionId\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"args\": {"]
#[doc = "      \"description\": \"Array of command arguments.\","]
#[doc = "      \"type\": \"array\","]
#[doc = "      \"items\": {"]
#[doc = "        \"type\": \"string\""]
#[doc = "      },"]
#[doc = "      \"x-deserialize-default-on-error\": true,"]
#[doc = "      \"x-deserialize-skip-invalid-items\": true"]
#[doc = "    },"]
#[doc = "    \"command\": {"]
#[doc = "      \"description\": \"The command to execute.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"cwd\": {"]
#[doc = "      \"description\": \"Working directory for the command. Must be an absolute path.\","]
#[doc = "      \"type\": ["]
#[doc = "        \"string\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"env\": {"]
#[doc = "      \"description\": \"Environment variables for the command.\","]
#[doc = "      \"type\": \"array\","]
#[doc = "      \"items\": {"]
#[doc = "        \"$ref\": \"#/$defs/EnvVariable\""]
#[doc = "      },"]
#[doc = "      \"x-deserialize-default-on-error\": true,"]
#[doc = "      \"x-deserialize-skip-invalid-items\": true"]
#[doc = "    },"]
#[doc = "    \"outputByteLimit\": {"]
#[doc = "      \"description\": \"Maximum number of output bytes to retain.\\n\\nWhen the limit is exceeded, the Client truncates from the beginning of the output\\nto stay within the limit.\\n\\nThe Client MUST ensure truncation happens at a character boundary to maintain valid\\nstring output, even if this means the retained output is slightly less than the\\nspecified limit.\","]
#[doc = "      \"type\": ["]
#[doc = "        \"integer\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"format\": \"uint64\","]
#[doc = "      \"minimum\": 0.0,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"sessionId\": {"]
#[doc = "      \"description\": \"The session ID for this request.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/SessionId\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"x-method\": \"terminal/create\","]
#[doc = "  \"x-side\": \"client\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct CreateTerminalRequest {
    #[doc = "Array of command arguments."]
    #[serde(default, skip_serializing_if = "::std::vec::Vec::is_empty")]
    pub args: ::std::vec::Vec<::std::string::String>,
    #[doc = "The command to execute."]
    pub command: ::std::string::String,
    #[doc = "Working directory for the command. Must be an absolute path."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub cwd: ::std::option::Option<::std::string::String>,
    #[doc = "Environment variables for the command."]
    #[serde(default, skip_serializing_if = "::std::vec::Vec::is_empty")]
    pub env: ::std::vec::Vec<EnvVariable>,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "Maximum number of output bytes to retain.\n\nWhen the limit is exceeded, the Client truncates from the beginning of the output\nto stay within the limit.\n\nThe Client MUST ensure truncation happens at a character boundary to maintain valid\nstring output, even if this means the retained output is slightly less than the\nspecified limit."]
    #[serde(
        rename = "outputByteLimit",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub output_byte_limit: ::std::option::Option<u64>,
    #[doc = "The session ID for this request."]
    #[serde(rename = "sessionId")]
    pub session_id: SessionId,
}
impl CreateTerminalRequest {
    pub fn builder() -> builder::CreateTerminalRequest {
        Default::default()
    }
}
#[doc = "Response containing the ID of the created terminal."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Response containing the ID of the created terminal.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"terminalId\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"terminalId\": {"]
#[doc = "      \"description\": \"The unique identifier for the created terminal.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/TerminalId\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"x-method\": \"terminal/create\","]
#[doc = "  \"x-side\": \"client\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct CreateTerminalResponse {
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "The unique identifier for the created terminal."]
    #[serde(rename = "terminalId")]
    pub terminal_id: TerminalId,
}
impl CreateTerminalResponse {
    pub fn builder() -> builder::CreateTerminalResponse {
        Default::default()
    }
}
#[doc = "The current mode of the session has changed\n\nSee protocol docs: [Session Modes](https://agentclientprotocol.com/protocol/session-modes)"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"The current mode of the session has changed\\n\\nSee protocol docs: [Session Modes](https://agentclientprotocol.com/protocol/session-modes)\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"currentModeId\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"currentModeId\": {"]
#[doc = "      \"description\": \"The ID of the current mode\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/SessionModeId\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct CurrentModeUpdate {
    #[doc = "The ID of the current mode"]
    #[serde(rename = "currentModeId")]
    pub current_mode_id: SessionModeId,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
}
impl CurrentModeUpdate {
    pub fn builder() -> builder::CurrentModeUpdate {
        Default::default()
    }
}
#[doc = "Request parameters for deleting an existing session from `session/list`.\n\nOnly available if the Agent supports the `sessionCapabilities.delete` capability."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Request parameters for deleting an existing session from `session/list`.\\n\\nOnly available if the Agent supports the `sessionCapabilities.delete` capability.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"sessionId\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"sessionId\": {"]
#[doc = "      \"description\": \"The ID of the session to delete.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/SessionId\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"x-method\": \"session/delete\","]
#[doc = "  \"x-side\": \"agent\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct DeleteSessionRequest {
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "The ID of the session to delete."]
    #[serde(rename = "sessionId")]
    pub session_id: SessionId,
}
impl DeleteSessionRequest {
    pub fn builder() -> builder::DeleteSessionRequest {
        Default::default()
    }
}
#[doc = "Response from deleting a session."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Response from deleting a session.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"x-method\": \"session/delete\","]
#[doc = "  \"x-side\": \"agent\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct DeleteSessionResponse {
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
}
impl ::std::default::Default for DeleteSessionResponse {
    fn default() -> Self {
        Self {
            meta: Default::default(),
        }
    }
}
impl DeleteSessionResponse {
    pub fn builder() -> builder::DeleteSessionResponse {
        Default::default()
    }
}
#[doc = "A diff representing file modifications.\n\nShows changes to files in a format suitable for display in the client UI.\n\nSee protocol docs: [Content](https://agentclientprotocol.com/protocol/tool-calls#content)"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"A diff representing file modifications.\\n\\nShows changes to files in a format suitable for display in the client UI.\\n\\nSee protocol docs: [Content](https://agentclientprotocol.com/protocol/tool-calls#content)\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"newText\","]
#[doc = "    \"path\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"newText\": {"]
#[doc = "      \"description\": \"The new content after modification.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"oldText\": {"]
#[doc = "      \"description\": \"The original content (None for new files).\","]
#[doc = "      \"type\": ["]
#[doc = "        \"string\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"path\": {"]
#[doc = "      \"description\": \"The absolute file path being modified.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct Diff {
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "The new content after modification."]
    #[serde(rename = "newText")]
    pub new_text: ::std::string::String,
    #[doc = "The original content (None for new files)."]
    #[serde(
        rename = "oldText",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub old_text: ::std::option::Option<::std::string::String>,
    #[doc = "The absolute file path being modified."]
    pub path: ::std::string::String,
}
impl Diff {
    pub fn builder() -> builder::Diff {
        Default::default()
    }
}
#[doc = "The contents of a resource, embedded into a prompt or tool call result."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"The contents of a resource, embedded into a prompt or tool call result.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"resource\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"annotations\": {"]
#[doc = "      \"description\": \"Optional annotations that help clients decide how to display or route this content.\","]
#[doc = "      \"anyOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/Annotations\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"type\": \"null\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"resource\": {"]
#[doc = "      \"description\": \"Embedded resource payload, either text or binary data.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/EmbeddedResourceResource\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct EmbeddedResource {
    #[doc = "Optional annotations that help clients decide how to display or route this content."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub annotations: ::std::option::Option<Annotations>,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "Embedded resource payload, either text or binary data."]
    pub resource: EmbeddedResourceResource,
}
impl EmbeddedResource {
    pub fn builder() -> builder::EmbeddedResource {
        Default::default()
    }
}
#[doc = "Resource content that can be embedded in a message."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Resource content that can be embedded in a message.\","]
#[doc = "  \"anyOf\": ["]
#[doc = "    {"]
#[doc = "      \"title\": \"TextResourceContents\","]
#[doc = "      \"description\": \"Text resource contents embedded directly in the message.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/TextResourceContents\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"BlobResourceContents\","]
#[doc = "      \"description\": \"Binary resource contents embedded directly in the message.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/BlobResourceContents\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct EmbeddedResourceResource {
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_0: ::std::option::Option<TextResourceContents>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_1: ::std::option::Option<BlobResourceContents>,
}
impl ::std::default::Default for EmbeddedResourceResource {
    fn default() -> Self {
        Self {
            subtype_0: Default::default(),
            subtype_1: Default::default(),
        }
    }
}
impl EmbeddedResourceResource {
    pub fn builder() -> builder::EmbeddedResourceResource {
        Default::default()
    }
}
#[doc = "An environment variable to set when launching an MCP server."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"An environment variable to set when launching an MCP server.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"name\","]
#[doc = "    \"value\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"name\": {"]
#[doc = "      \"description\": \"The name of the environment variable.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"value\": {"]
#[doc = "      \"description\": \"The value to set for the environment variable.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct EnvVariable {
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "The name of the environment variable."]
    pub name: ::std::string::String,
    #[doc = "The value to set for the environment variable."]
    pub value: ::std::string::String,
}
impl EnvVariable {
    pub fn builder() -> builder::EnvVariable {
        Default::default()
    }
}
#[doc = "JSON-RPC error object.\n\nRepresents an error that occurred during method execution, following the\nJSON-RPC 2.0 error object specification with optional additional data.\n\nSee protocol docs: [JSON-RPC Error Object](https://www.jsonrpc.org/specification#error_object)"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"JSON-RPC error object.\\n\\nRepresents an error that occurred during method execution, following the\\nJSON-RPC 2.0 error object specification with optional additional data.\\n\\nSee protocol docs: [JSON-RPC Error Object](https://www.jsonrpc.org/specification#error_object)\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"code\","]
#[doc = "    \"message\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"code\": {"]
#[doc = "      \"description\": \"A number indicating the error type that occurred.\\nThis must be an integer as defined in the JSON-RPC specification.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/ErrorCode\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    \"data\": {"]
#[doc = "      \"description\": \"Optional primitive or structured value that contains additional information about the error.\\nThis may include debugging information or context-specific details.\","]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"message\": {"]
#[doc = "      \"description\": \"A string providing a short description of the error.\\nThe message should be limited to a concise single sentence.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct Error {
    #[doc = "A number indicating the error type that occurred.\nThis must be an integer as defined in the JSON-RPC specification."]
    pub code: ErrorCode,
    #[doc = "Optional primitive or structured value that contains additional information about the error.\nThis may include debugging information or context-specific details."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub data: ::std::option::Option<::serde_json::Value>,
    #[doc = "A string providing a short description of the error.\nThe message should be limited to a concise single sentence."]
    pub message: ::std::string::String,
}
impl Error {
    pub fn builder() -> builder::Error {
        Default::default()
    }
}
#[doc = "Predefined error codes for common JSON-RPC and ACP-specific errors.\n\nThese codes follow the JSON-RPC 2.0 specification for standard errors\nand use the reserved range (-32000 to -32099) for protocol-specific errors."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Predefined error codes for common JSON-RPC and ACP-specific errors.\\n\\nThese codes follow the JSON-RPC 2.0 specification for standard errors\\nand use the reserved range (-32000 to -32099) for protocol-specific errors.\","]
#[doc = "  \"anyOf\": ["]
#[doc = "    {"]
#[doc = "      \"title\": \"Parse error\","]
#[doc = "      \"description\": \"**Parse error**: Invalid JSON was received by the server.\\nAn error occurred on the server while parsing the JSON text.\","]
#[doc = "      \"type\": \"integer\","]
#[doc = "      \"format\": \"int32\","]
#[doc = "      \"const\": -32700"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"Invalid request\","]
#[doc = "      \"description\": \"**Invalid request**: The JSON sent is not a valid Request object.\","]
#[doc = "      \"type\": \"integer\","]
#[doc = "      \"format\": \"int32\","]
#[doc = "      \"const\": -32600"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"Method not found\","]
#[doc = "      \"description\": \"**Method not found**: The method does not exist or is not available.\","]
#[doc = "      \"type\": \"integer\","]
#[doc = "      \"format\": \"int32\","]
#[doc = "      \"const\": -32601"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"Invalid params\","]
#[doc = "      \"description\": \"**Invalid params**: Invalid method parameter(s).\","]
#[doc = "      \"type\": \"integer\","]
#[doc = "      \"format\": \"int32\","]
#[doc = "      \"const\": -32602"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"Internal error\","]
#[doc = "      \"description\": \"**Internal error**: Internal JSON-RPC error.\\nReserved for implementation-defined server errors.\","]
#[doc = "      \"type\": \"integer\","]
#[doc = "      \"format\": \"int32\","]
#[doc = "      \"const\": -32603"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"Request cancelled\","]
#[doc = "      \"description\": \"**Request cancelled**: Execution of the method was aborted either due to a cancellation request from the caller or\\nbecause of resource constraints or shutdown.\","]
#[doc = "      \"type\": \"integer\","]
#[doc = "      \"format\": \"int32\","]
#[doc = "      \"const\": -32800"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"Authentication required\","]
#[doc = "      \"description\": \"**Authentication required**: Authentication is required before this operation can be performed.\","]
#[doc = "      \"type\": \"integer\","]
#[doc = "      \"format\": \"int32\","]
#[doc = "      \"const\": -32000"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"Resource not found\","]
#[doc = "      \"description\": \"**Resource not found**: A given resource, such as a file, was not found.\","]
#[doc = "      \"type\": \"integer\","]
#[doc = "      \"format\": \"int32\","]
#[doc = "      \"const\": -32002"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"Other\","]
#[doc = "      \"description\": \"Other undefined error code.\","]
#[doc = "      \"type\": \"integer\","]
#[doc = "      \"format\": \"int32\""]
#[doc = "    }"]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct ErrorCode {
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_0: ::std::option::Option<i32>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_1: ::std::option::Option<i32>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_2: ::std::option::Option<i32>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_3: ::std::option::Option<i32>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_4: ::std::option::Option<i32>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_5: ::std::option::Option<i32>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_6: ::std::option::Option<i32>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_7: ::std::option::Option<i32>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_8: ::std::option::Option<i32>,
}
impl ::std::default::Default for ErrorCode {
    fn default() -> Self {
        Self {
            subtype_0: Default::default(),
            subtype_1: Default::default(),
            subtype_2: Default::default(),
            subtype_3: Default::default(),
            subtype_4: Default::default(),
            subtype_5: Default::default(),
            subtype_6: Default::default(),
            subtype_7: Default::default(),
            subtype_8: Default::default(),
        }
    }
}
impl ErrorCode {
    pub fn builder() -> builder::ErrorCode {
        Default::default()
    }
}
#[doc = "Allows the Agent to send an arbitrary notification that is not part of the ACP spec.\nExtension notifications provide a way to send one-way messages for custom functionality\nwhile maintaining protocol compatibility.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Allows the Agent to send an arbitrary notification that is not part of the ACP spec.\\nExtension notifications provide a way to send one-way messages for custom functionality\\nwhile maintaining protocol compatibility.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
#[serde(transparent)]
pub struct ExtNotification(pub ::serde_json::Value);
impl ::std::ops::Deref for ExtNotification {
    type Target = ::serde_json::Value;
    fn deref(&self) -> &::serde_json::Value {
        &self.0
    }
}
impl ::std::convert::From<ExtNotification> for ::serde_json::Value {
    fn from(value: ExtNotification) -> Self {
        value.0
    }
}
impl ::std::convert::From<::serde_json::Value> for ExtNotification {
    fn from(value: ::serde_json::Value) -> Self {
        Self(value)
    }
}
#[doc = "Allows for sending an arbitrary request that is not part of the ACP spec.\nExtension methods provide a way to add custom functionality while maintaining\nprotocol compatibility.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Allows for sending an arbitrary request that is not part of the ACP spec.\\nExtension methods provide a way to add custom functionality while maintaining\\nprotocol compatibility.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
#[serde(transparent)]
pub struct ExtRequest(pub ::serde_json::Value);
impl ::std::ops::Deref for ExtRequest {
    type Target = ::serde_json::Value;
    fn deref(&self) -> &::serde_json::Value {
        &self.0
    }
}
impl ::std::convert::From<ExtRequest> for ::serde_json::Value {
    fn from(value: ExtRequest) -> Self {
        value.0
    }
}
impl ::std::convert::From<::serde_json::Value> for ExtRequest {
    fn from(value: ::serde_json::Value) -> Self {
        Self(value)
    }
}
#[doc = "Allows for sending an arbitrary response to an [`ExtRequest`] that is not part of the ACP spec.\nExtension methods provide a way to add custom functionality while maintaining\nprotocol compatibility.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Allows for sending an arbitrary response to an [`ExtRequest`] that is not part of the ACP spec.\\nExtension methods provide a way to add custom functionality while maintaining\\nprotocol compatibility.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
#[serde(transparent)]
pub struct ExtResponse(pub ::serde_json::Value);
impl ::std::ops::Deref for ExtResponse {
    type Target = ::serde_json::Value;
    fn deref(&self) -> &::serde_json::Value {
        &self.0
    }
}
impl ::std::convert::From<ExtResponse> for ::serde_json::Value {
    fn from(value: ExtResponse) -> Self {
        value.0
    }
}
impl ::std::convert::From<::serde_json::Value> for ExtResponse {
    fn from(value: ::serde_json::Value) -> Self {
        Self(value)
    }
}
#[doc = "File system capabilities that a client may support.\n\nSee protocol docs: [FileSystem](https://agentclientprotocol.com/protocol/initialization#filesystem)"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"File system capabilities that a client may support.\\n\\nSee protocol docs: [FileSystem](https://agentclientprotocol.com/protocol/initialization#filesystem)\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"readTextFile\": {"]
#[doc = "      \"description\": \"Whether the Client supports `fs/read_text_file` requests.\","]
#[doc = "      \"default\": false,"]
#[doc = "      \"type\": \"boolean\","]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"writeTextFile\": {"]
#[doc = "      \"description\": \"Whether the Client supports `fs/write_text_file` requests.\","]
#[doc = "      \"default\": false,"]
#[doc = "      \"type\": \"boolean\","]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct FileSystemCapabilities {
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "Whether the Client supports `fs/read_text_file` requests."]
    #[serde(rename = "readTextFile", default)]
    pub read_text_file: bool,
    #[doc = "Whether the Client supports `fs/write_text_file` requests."]
    #[serde(rename = "writeTextFile", default)]
    pub write_text_file: bool,
}
impl ::std::default::Default for FileSystemCapabilities {
    fn default() -> Self {
        Self {
            meta: Default::default(),
            read_text_file: Default::default(),
            write_text_file: Default::default(),
        }
    }
}
impl FileSystemCapabilities {
    pub fn builder() -> builder::FileSystemCapabilities {
        Default::default()
    }
}
#[doc = "An HTTP header to set when making requests to the MCP server."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"An HTTP header to set when making requests to the MCP server.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"name\","]
#[doc = "    \"value\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"name\": {"]
#[doc = "      \"description\": \"The name of the HTTP header.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"value\": {"]
#[doc = "      \"description\": \"The value to set for the HTTP header.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct HttpHeader {
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "The name of the HTTP header."]
    pub name: ::std::string::String,
    #[doc = "The value to set for the HTTP header."]
    pub value: ::std::string::String,
}
impl HttpHeader {
    pub fn builder() -> builder::HttpHeader {
        Default::default()
    }
}
#[doc = "An image provided to or from an LLM."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"An image provided to or from an LLM.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"data\","]
#[doc = "    \"mimeType\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"annotations\": {"]
#[doc = "      \"description\": \"Optional annotations that help clients decide how to display or route this content.\","]
#[doc = "      \"anyOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/Annotations\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"type\": \"null\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"data\": {"]
#[doc = "      \"description\": \"Base64-encoded media payload.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"mimeType\": {"]
#[doc = "      \"description\": \"MIME type describing the encoded media payload.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"uri\": {"]
#[doc = "      \"description\": \"URI associated with this resource or media payload.\","]
#[doc = "      \"type\": ["]
#[doc = "        \"string\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct ImageContent {
    #[doc = "Optional annotations that help clients decide how to display or route this content."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub annotations: ::std::option::Option<Annotations>,
    #[doc = "Base64-encoded media payload."]
    pub data: ::std::string::String,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "MIME type describing the encoded media payload."]
    #[serde(rename = "mimeType")]
    pub mime_type: ::std::string::String,
    #[doc = "URI associated with this resource or media payload."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub uri: ::std::option::Option<::std::string::String>,
}
impl ImageContent {
    pub fn builder() -> builder::ImageContent {
        Default::default()
    }
}
#[doc = "Metadata about the implementation of the client or agent.\nDescribes the name and version of an ACP implementation, with an optional\ntitle for UI representation."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Metadata about the implementation of the client or agent.\\nDescribes the name and version of an ACP implementation, with an optional\\ntitle for UI representation.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"name\","]
#[doc = "    \"version\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"name\": {"]
#[doc = "      \"description\": \"Intended for programmatic or logical use, but can be used as a display\\nname fallback if title isn’t present.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"title\": {"]
#[doc = "      \"description\": \"Intended for UI and end-user contexts — optimized to be human-readable\\nand easily understood.\\n\\nIf not provided, the name should be used for display.\","]
#[doc = "      \"type\": ["]
#[doc = "        \"string\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"version\": {"]
#[doc = "      \"description\": \"Version of the implementation. Can be displayed to the user or used\\nfor debugging or metrics purposes. (e.g. \\\"1.0.0\\\").\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct Implementation {
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "Intended for programmatic or logical use, but can be used as a display\nname fallback if title isn’t present."]
    pub name: ::std::string::String,
    #[doc = "Intended for UI and end-user contexts — optimized to be human-readable\nand easily understood.\n\nIf not provided, the name should be used for display."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub title: ::std::option::Option<::std::string::String>,
    #[doc = "Version of the implementation. Can be displayed to the user or used\nfor debugging or metrics purposes. (e.g. \"1.0.0\")."]
    pub version: ::std::string::String,
}
impl Implementation {
    pub fn builder() -> builder::Implementation {
        Default::default()
    }
}
#[doc = "Request parameters for the initialize method.\n\nSent by the client to establish connection and negotiate capabilities.\n\nSee protocol docs: [Initialization](https://agentclientprotocol.com/protocol/initialization)"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Request parameters for the initialize method.\\n\\nSent by the client to establish connection and negotiate capabilities.\\n\\nSee protocol docs: [Initialization](https://agentclientprotocol.com/protocol/initialization)\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"protocolVersion\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"clientCapabilities\": {"]
#[doc = "      \"description\": \"Capabilities supported by the client.\","]
#[doc = "      \"default\": {"]
#[doc = "        \"fs\": {"]
#[doc = "          \"readTextFile\": false,"]
#[doc = "          \"writeTextFile\": false"]
#[doc = "        },"]
#[doc = "        \"terminal\": false"]
#[doc = "      },"]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/ClientCapabilities\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"clientInfo\": {"]
#[doc = "      \"description\": \"Information about the Client name and version sent to the Agent.\\n\\nNote: in future versions of the protocol, this will be required.\","]
#[doc = "      \"anyOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/Implementation\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"type\": \"null\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"protocolVersion\": {"]
#[doc = "      \"description\": \"The latest protocol version supported by the client.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/ProtocolVersion\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"x-method\": \"initialize\","]
#[doc = "  \"x-side\": \"agent\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct InitializeRequest {
    #[doc = "Capabilities supported by the client."]
    #[serde(
        rename = "clientCapabilities",
        default = "defaults::initialize_request_client_capabilities"
    )]
    pub client_capabilities: ClientCapabilities,
    #[doc = "Information about the Client name and version sent to the Agent.\n\nNote: in future versions of the protocol, this will be required."]
    #[serde(
        rename = "clientInfo",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub client_info: ::std::option::Option<Implementation>,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "The latest protocol version supported by the client."]
    #[serde(rename = "protocolVersion")]
    pub protocol_version: ProtocolVersion,
}
impl InitializeRequest {
    pub fn builder() -> builder::InitializeRequest {
        Default::default()
    }
}
#[doc = "Response to the `initialize` method.\n\nContains the negotiated protocol version and agent capabilities.\n\nSee protocol docs: [Initialization](https://agentclientprotocol.com/protocol/initialization)"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Response to the `initialize` method.\\n\\nContains the negotiated protocol version and agent capabilities.\\n\\nSee protocol docs: [Initialization](https://agentclientprotocol.com/protocol/initialization)\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"protocolVersion\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"agentCapabilities\": {"]
#[doc = "      \"description\": \"Capabilities supported by the agent.\","]
#[doc = "      \"default\": {"]
#[doc = "        \"auth\": {},"]
#[doc = "        \"loadSession\": false,"]
#[doc = "        \"mcpCapabilities\": {"]
#[doc = "          \"http\": false,"]
#[doc = "          \"sse\": false"]
#[doc = "        },"]
#[doc = "        \"promptCapabilities\": {"]
#[doc = "          \"audio\": false,"]
#[doc = "          \"embeddedContext\": false,"]
#[doc = "          \"image\": false"]
#[doc = "        },"]
#[doc = "        \"sessionCapabilities\": {}"]
#[doc = "      },"]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/AgentCapabilities\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"agentInfo\": {"]
#[doc = "      \"description\": \"Information about the Agent name and version sent to the Client.\\n\\nNote: in future versions of the protocol, this will be required.\","]
#[doc = "      \"anyOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/Implementation\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"type\": \"null\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"authMethods\": {"]
#[doc = "      \"description\": \"Authentication methods supported by the agent.\","]
#[doc = "      \"default\": [],"]
#[doc = "      \"type\": \"array\","]
#[doc = "      \"items\": {"]
#[doc = "        \"$ref\": \"#/$defs/AuthMethod\""]
#[doc = "      },"]
#[doc = "      \"x-deserialize-default-on-error\": true,"]
#[doc = "      \"x-deserialize-skip-invalid-items\": true"]
#[doc = "    },"]
#[doc = "    \"protocolVersion\": {"]
#[doc = "      \"description\": \"The protocol version the client specified if supported by the agent,\\nor the latest protocol version supported by the agent.\\n\\nThe client should disconnect, if it doesn't support this version.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/ProtocolVersion\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"x-method\": \"initialize\","]
#[doc = "  \"x-side\": \"agent\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct InitializeResponse {
    #[doc = "Capabilities supported by the agent."]
    #[serde(
        rename = "agentCapabilities",
        default = "defaults::initialize_response_agent_capabilities"
    )]
    pub agent_capabilities: AgentCapabilities,
    #[doc = "Information about the Agent name and version sent to the Client.\n\nNote: in future versions of the protocol, this will be required."]
    #[serde(
        rename = "agentInfo",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub agent_info: ::std::option::Option<Implementation>,
    #[doc = "Authentication methods supported by the agent."]
    #[serde(
        rename = "authMethods",
        default,
        skip_serializing_if = "::std::vec::Vec::is_empty"
    )]
    pub auth_methods: ::std::vec::Vec<AuthMethod>,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "The protocol version the client specified if supported by the agent,\nor the latest protocol version supported by the agent.\n\nThe client should disconnect, if it doesn't support this version."]
    #[serde(rename = "protocolVersion")]
    pub protocol_version: ProtocolVersion,
}
impl InitializeResponse {
    pub fn builder() -> builder::InitializeResponse {
        Default::default()
    }
}
#[doc = "Request to kill a terminal without releasing it."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Request to kill a terminal without releasing it.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"sessionId\","]
#[doc = "    \"terminalId\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"sessionId\": {"]
#[doc = "      \"description\": \"The session ID for this request.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/SessionId\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    \"terminalId\": {"]
#[doc = "      \"description\": \"The ID of the terminal to kill.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/TerminalId\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"x-method\": \"terminal/kill\","]
#[doc = "  \"x-side\": \"client\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct KillTerminalRequest {
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "The session ID for this request."]
    #[serde(rename = "sessionId")]
    pub session_id: SessionId,
    #[doc = "The ID of the terminal to kill."]
    #[serde(rename = "terminalId")]
    pub terminal_id: TerminalId,
}
impl KillTerminalRequest {
    pub fn builder() -> builder::KillTerminalRequest {
        Default::default()
    }
}
#[doc = "Response to `terminal/kill` method"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Response to `terminal/kill` method\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"x-method\": \"terminal/kill\","]
#[doc = "  \"x-side\": \"client\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct KillTerminalResponse {
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
}
impl ::std::default::Default for KillTerminalResponse {
    fn default() -> Self {
        Self {
            meta: Default::default(),
        }
    }
}
impl KillTerminalResponse {
    pub fn builder() -> builder::KillTerminalResponse {
        Default::default()
    }
}
#[doc = "Request parameters for listing existing sessions.\n\nOnly available if the Agent supports the `sessionCapabilities.list` capability."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Request parameters for listing existing sessions.\\n\\nOnly available if the Agent supports the `sessionCapabilities.list` capability.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"cursor\": {"]
#[doc = "      \"description\": \"Opaque cursor token from a previous response's nextCursor field for cursor-based pagination\","]
#[doc = "      \"type\": ["]
#[doc = "        \"string\","]
#[doc = "        \"null\""]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    \"cwd\": {"]
#[doc = "      \"description\": \"Filter sessions by working directory. Must be an absolute path.\","]
#[doc = "      \"type\": ["]
#[doc = "        \"string\","]
#[doc = "        \"null\""]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"x-method\": \"session/list\","]
#[doc = "  \"x-side\": \"agent\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct ListSessionsRequest {
    #[doc = "Opaque cursor token from a previous response's nextCursor field for cursor-based pagination"]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub cursor: ::std::option::Option<::std::string::String>,
    #[doc = "Filter sessions by working directory. Must be an absolute path."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub cwd: ::std::option::Option<::std::string::String>,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
}
impl ::std::default::Default for ListSessionsRequest {
    fn default() -> Self {
        Self {
            cursor: Default::default(),
            cwd: Default::default(),
            meta: Default::default(),
        }
    }
}
impl ListSessionsRequest {
    pub fn builder() -> builder::ListSessionsRequest {
        Default::default()
    }
}
#[doc = "Response from listing sessions."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Response from listing sessions.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"sessions\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"nextCursor\": {"]
#[doc = "      \"description\": \"Opaque cursor token. If present, pass this in the next request's cursor parameter\\nto fetch the next page. If absent, there are no more results.\","]
#[doc = "      \"type\": ["]
#[doc = "        \"string\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"sessions\": {"]
#[doc = "      \"description\": \"Array of session information objects\","]
#[doc = "      \"type\": \"array\","]
#[doc = "      \"items\": {"]
#[doc = "        \"$ref\": \"#/$defs/SessionInfo\""]
#[doc = "      },"]
#[doc = "      \"x-deserialize-default-on-error\": true,"]
#[doc = "      \"x-deserialize-skip-invalid-items\": true"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"x-method\": \"session/list\","]
#[doc = "  \"x-side\": \"agent\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct ListSessionsResponse {
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "Opaque cursor token. If present, pass this in the next request's cursor parameter\nto fetch the next page. If absent, there are no more results."]
    #[serde(
        rename = "nextCursor",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub next_cursor: ::std::option::Option<::std::string::String>,
    #[doc = "Array of session information objects"]
    pub sessions: ::std::vec::Vec<SessionInfo>,
}
impl ListSessionsResponse {
    pub fn builder() -> builder::ListSessionsResponse {
        Default::default()
    }
}
#[doc = "Request parameters for loading an existing session.\n\nOnly available if the Agent supports the `loadSession` capability.\n\nSee protocol docs: [Loading Sessions](https://agentclientprotocol.com/protocol/session-setup#loading-sessions)"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Request parameters for loading an existing session.\\n\\nOnly available if the Agent supports the `loadSession` capability.\\n\\nSee protocol docs: [Loading Sessions](https://agentclientprotocol.com/protocol/session-setup#loading-sessions)\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"cwd\","]
#[doc = "    \"mcpServers\","]
#[doc = "    \"sessionId\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"additionalDirectories\": {"]
#[doc = "      \"description\": \"Additional workspace roots to activate for this session. Each path must be absolute.\\n\\nWhen omitted or empty, no additional roots are activated. When non-empty,\\nthis is the complete resulting additional-root list for the loaded\\nsession. It may differ from any previously used or reported list as long as\\nthe request `cwd` matches the session's `cwd`.\","]
#[doc = "      \"type\": \"array\","]
#[doc = "      \"items\": {"]
#[doc = "        \"type\": \"string\""]
#[doc = "      },"]
#[doc = "      \"x-deserialize-default-on-error\": true,"]
#[doc = "      \"x-deserialize-skip-invalid-items\": true"]
#[doc = "    },"]
#[doc = "    \"cwd\": {"]
#[doc = "      \"description\": \"The working directory for this session. Must be an absolute path.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"mcpServers\": {"]
#[doc = "      \"description\": \"List of MCP servers to connect to for this session.\","]
#[doc = "      \"type\": \"array\","]
#[doc = "      \"items\": {"]
#[doc = "        \"$ref\": \"#/$defs/McpServer\""]
#[doc = "      },"]
#[doc = "      \"x-deserialize-default-on-error\": true,"]
#[doc = "      \"x-deserialize-skip-invalid-items\": true"]
#[doc = "    },"]
#[doc = "    \"sessionId\": {"]
#[doc = "      \"description\": \"The ID of the session to load.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/SessionId\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"x-method\": \"session/load\","]
#[doc = "  \"x-side\": \"agent\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct LoadSessionRequest {
    #[doc = "Additional workspace roots to activate for this session. Each path must be absolute.\n\nWhen omitted or empty, no additional roots are activated. When non-empty,\nthis is the complete resulting additional-root list for the loaded\nsession. It may differ from any previously used or reported list as long as\nthe request `cwd` matches the session's `cwd`."]
    #[serde(
        rename = "additionalDirectories",
        default,
        skip_serializing_if = "::std::vec::Vec::is_empty"
    )]
    pub additional_directories: ::std::vec::Vec<::std::string::String>,
    #[doc = "The working directory for this session. Must be an absolute path."]
    pub cwd: ::std::string::String,
    #[doc = "List of MCP servers to connect to for this session."]
    #[serde(rename = "mcpServers")]
    pub mcp_servers: ::std::vec::Vec<McpServer>,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "The ID of the session to load."]
    #[serde(rename = "sessionId")]
    pub session_id: SessionId,
}
impl LoadSessionRequest {
    pub fn builder() -> builder::LoadSessionRequest {
        Default::default()
    }
}
#[doc = "Response from loading an existing session."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Response from loading an existing session.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"configOptions\": {"]
#[doc = "      \"description\": \"Initial session configuration options if supported by the Agent.\","]
#[doc = "      \"type\": ["]
#[doc = "        \"array\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"items\": {"]
#[doc = "        \"$ref\": \"#/$defs/SessionConfigOption\""]
#[doc = "      },"]
#[doc = "      \"x-deserialize-default-on-error\": true,"]
#[doc = "      \"x-deserialize-skip-invalid-items\": true"]
#[doc = "    },"]
#[doc = "    \"modes\": {"]
#[doc = "      \"description\": \"Initial mode state if supported by the Agent\\n\\nSee protocol docs: [Session Modes](https://agentclientprotocol.com/protocol/session-modes)\","]
#[doc = "      \"anyOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/SessionModeState\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"type\": \"null\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"x-method\": \"session/load\","]
#[doc = "  \"x-side\": \"agent\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct LoadSessionResponse {
    #[doc = "Initial session configuration options if supported by the Agent."]
    #[serde(
        rename = "configOptions",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub config_options: ::std::option::Option<::std::vec::Vec<SessionConfigOption>>,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "Initial mode state if supported by the Agent\n\nSee protocol docs: [Session Modes](https://agentclientprotocol.com/protocol/session-modes)"]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub modes: ::std::option::Option<SessionModeState>,
}
impl ::std::default::Default for LoadSessionResponse {
    fn default() -> Self {
        Self {
            config_options: Default::default(),
            meta: Default::default(),
            modes: Default::default(),
        }
    }
}
impl LoadSessionResponse {
    pub fn builder() -> builder::LoadSessionResponse {
        Default::default()
    }
}
#[doc = "Logout capabilities supported by the agent.\n\nSupplying `{}` means the agent supports the logout method."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Logout capabilities supported by the agent.\\n\\nSupplying `{}` means the agent supports the logout method.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct LogoutCapabilities {
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
}
impl ::std::default::Default for LogoutCapabilities {
    fn default() -> Self {
        Self {
            meta: Default::default(),
        }
    }
}
impl LogoutCapabilities {
    pub fn builder() -> builder::LogoutCapabilities {
        Default::default()
    }
}
#[doc = "Request parameters for the logout method.\n\nTerminates the current authenticated session."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Request parameters for the logout method.\\n\\nTerminates the current authenticated session.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"x-method\": \"logout\","]
#[doc = "  \"x-side\": \"agent\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct LogoutRequest {
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
}
impl ::std::default::Default for LogoutRequest {
    fn default() -> Self {
        Self {
            meta: Default::default(),
        }
    }
}
impl LogoutRequest {
    pub fn builder() -> builder::LogoutRequest {
        Default::default()
    }
}
#[doc = "Response to the `logout` method."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Response to the `logout` method.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"x-method\": \"logout\","]
#[doc = "  \"x-side\": \"agent\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct LogoutResponse {
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
}
impl ::std::default::Default for LogoutResponse {
    fn default() -> Self {
        Self {
            meta: Default::default(),
        }
    }
}
impl LogoutResponse {
    pub fn builder() -> builder::LogoutResponse {
        Default::default()
    }
}
#[doc = "MCP capabilities supported by the agent"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"MCP capabilities supported by the agent\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"http\": {"]
#[doc = "      \"description\": \"Agent supports [`McpServer::Http`].\","]
#[doc = "      \"default\": false,"]
#[doc = "      \"type\": \"boolean\","]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"sse\": {"]
#[doc = "      \"description\": \"Agent supports [`McpServer::Sse`].\","]
#[doc = "      \"default\": false,"]
#[doc = "      \"type\": \"boolean\","]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct McpCapabilities {
    #[doc = "Agent supports [`McpServer::Http`]."]
    #[serde(default)]
    pub http: bool,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "Agent supports [`McpServer::Sse`]."]
    #[serde(default)]
    pub sse: bool,
}
impl ::std::default::Default for McpCapabilities {
    fn default() -> Self {
        Self {
            http: Default::default(),
            meta: Default::default(),
            sse: Default::default(),
        }
    }
}
impl McpCapabilities {
    pub fn builder() -> builder::McpCapabilities {
        Default::default()
    }
}
#[doc = "Configuration for connecting to an MCP (Model Context Protocol) server.\n\nMCP servers provide tools and context that the agent can use when\nprocessing prompts.\n\nSee protocol docs: [MCP Servers](https://agentclientprotocol.com/protocol/session-setup#mcp-servers)"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Configuration for connecting to an MCP (Model Context Protocol) server.\\n\\nMCP servers provide tools and context that the agent can use when\\nprocessing prompts.\\n\\nSee protocol docs: [MCP Servers](https://agentclientprotocol.com/protocol/session-setup#mcp-servers)\","]
#[doc = "  \"anyOf\": ["]
#[doc = "    {"]
#[doc = "      \"description\": \"HTTP transport configuration\\n\\nOnly available when the Agent capabilities indicate `mcp_capabilities.http` is `true`.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/McpServerHttp\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"required\": ["]
#[doc = "        \"type\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"type\": {"]
#[doc = "          \"type\": \"string\","]
#[doc = "          \"const\": \"http\""]
#[doc = "        }"]
#[doc = "      }"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"description\": \"SSE transport configuration\\n\\nOnly available when the Agent capabilities indicate `mcp_capabilities.sse` is `true`.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/McpServerSse\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"required\": ["]
#[doc = "        \"type\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"type\": {"]
#[doc = "          \"type\": \"string\","]
#[doc = "          \"const\": \"sse\""]
#[doc = "        }"]
#[doc = "      }"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"stdio\","]
#[doc = "      \"description\": \"Stdio transport configuration\\n\\nAll Agents MUST support this transport.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/McpServerStdio\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct McpServer {
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_0: ::std::option::Option<McpServerSubtype0>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_1: ::std::option::Option<McpServerSubtype1>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_2: ::std::option::Option<McpServerStdio>,
}
impl ::std::default::Default for McpServer {
    fn default() -> Self {
        Self {
            subtype_0: Default::default(),
            subtype_1: Default::default(),
            subtype_2: Default::default(),
        }
    }
}
impl McpServer {
    pub fn builder() -> builder::McpServer {
        Default::default()
    }
}
#[doc = "HTTP transport configuration for MCP."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"HTTP transport configuration for MCP.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"headers\","]
#[doc = "    \"name\","]
#[doc = "    \"url\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"headers\": {"]
#[doc = "      \"description\": \"HTTP headers to set when making requests to the MCP server.\","]
#[doc = "      \"type\": \"array\","]
#[doc = "      \"items\": {"]
#[doc = "        \"$ref\": \"#/$defs/HttpHeader\""]
#[doc = "      }"]
#[doc = "    },"]
#[doc = "    \"name\": {"]
#[doc = "      \"description\": \"Human-readable name identifying this MCP server.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"url\": {"]
#[doc = "      \"description\": \"URL to the MCP server.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct McpServerHttp {
    #[doc = "HTTP headers to set when making requests to the MCP server."]
    pub headers: ::std::vec::Vec<HttpHeader>,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "Human-readable name identifying this MCP server."]
    pub name: ::std::string::String,
    #[doc = "URL to the MCP server."]
    pub url: ::std::string::String,
}
impl McpServerHttp {
    pub fn builder() -> builder::McpServerHttp {
        Default::default()
    }
}
#[doc = "SSE transport configuration for MCP."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"SSE transport configuration for MCP.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"headers\","]
#[doc = "    \"name\","]
#[doc = "    \"url\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"headers\": {"]
#[doc = "      \"description\": \"HTTP headers to set when making requests to the MCP server.\","]
#[doc = "      \"type\": \"array\","]
#[doc = "      \"items\": {"]
#[doc = "        \"$ref\": \"#/$defs/HttpHeader\""]
#[doc = "      }"]
#[doc = "    },"]
#[doc = "    \"name\": {"]
#[doc = "      \"description\": \"Human-readable name identifying this MCP server.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"url\": {"]
#[doc = "      \"description\": \"URL to the MCP server.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct McpServerSse {
    #[doc = "HTTP headers to set when making requests to the MCP server."]
    pub headers: ::std::vec::Vec<HttpHeader>,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "Human-readable name identifying this MCP server."]
    pub name: ::std::string::String,
    #[doc = "URL to the MCP server."]
    pub url: ::std::string::String,
}
impl McpServerSse {
    pub fn builder() -> builder::McpServerSse {
        Default::default()
    }
}
#[doc = "Stdio transport configuration for MCP."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Stdio transport configuration for MCP.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"args\","]
#[doc = "    \"command\","]
#[doc = "    \"env\","]
#[doc = "    \"name\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"args\": {"]
#[doc = "      \"description\": \"Command-line arguments to pass to the MCP server.\","]
#[doc = "      \"type\": \"array\","]
#[doc = "      \"items\": {"]
#[doc = "        \"type\": \"string\""]
#[doc = "      }"]
#[doc = "    },"]
#[doc = "    \"command\": {"]
#[doc = "      \"description\": \"Absolute path to the MCP server executable.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"env\": {"]
#[doc = "      \"description\": \"Environment variables to set when launching the MCP server.\","]
#[doc = "      \"type\": \"array\","]
#[doc = "      \"items\": {"]
#[doc = "        \"$ref\": \"#/$defs/EnvVariable\""]
#[doc = "      }"]
#[doc = "    },"]
#[doc = "    \"name\": {"]
#[doc = "      \"description\": \"Human-readable name identifying this MCP server.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct McpServerStdio {
    #[doc = "Command-line arguments to pass to the MCP server."]
    pub args: ::std::vec::Vec<::std::string::String>,
    #[doc = "Absolute path to the MCP server executable."]
    pub command: ::std::string::String,
    #[doc = "Environment variables to set when launching the MCP server."]
    pub env: ::std::vec::Vec<EnvVariable>,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "Human-readable name identifying this MCP server."]
    pub name: ::std::string::String,
}
impl McpServerStdio {
    pub fn builder() -> builder::McpServerStdio {
        Default::default()
    }
}
#[doc = "HTTP transport configuration\n\nOnly available when the Agent capabilities indicate `mcp_capabilities.http` is `true`."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"HTTP transport configuration\\n\\nOnly available when the Agent capabilities indicate `mcp_capabilities.http` is `true`.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"allOf\": ["]
#[doc = "    {"]
#[doc = "      \"$ref\": \"#/$defs/McpServerHttp\""]
#[doc = "    }"]
#[doc = "  ],"]
#[doc = "  \"required\": ["]
#[doc = "    \"type\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"type\": {"]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"const\": \"http\""]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct McpServerSubtype0 {
    #[doc = "HTTP headers to set when making requests to the MCP server."]
    pub headers: ::std::vec::Vec<HttpHeader>,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "Human-readable name identifying this MCP server."]
    pub name: ::std::string::String,
    #[serde(rename = "type")]
    pub type_: ::std::string::String,
    #[doc = "URL to the MCP server."]
    pub url: ::std::string::String,
}
impl McpServerSubtype0 {
    pub fn builder() -> builder::McpServerSubtype0 {
        Default::default()
    }
}
#[doc = "SSE transport configuration\n\nOnly available when the Agent capabilities indicate `mcp_capabilities.sse` is `true`."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"SSE transport configuration\\n\\nOnly available when the Agent capabilities indicate `mcp_capabilities.sse` is `true`.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"allOf\": ["]
#[doc = "    {"]
#[doc = "      \"$ref\": \"#/$defs/McpServerSse\""]
#[doc = "    }"]
#[doc = "  ],"]
#[doc = "  \"required\": ["]
#[doc = "    \"type\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"type\": {"]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"const\": \"sse\""]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct McpServerSubtype1 {
    #[doc = "HTTP headers to set when making requests to the MCP server."]
    pub headers: ::std::vec::Vec<HttpHeader>,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "Human-readable name identifying this MCP server."]
    pub name: ::std::string::String,
    #[serde(rename = "type")]
    pub type_: ::std::string::String,
    #[doc = "URL to the MCP server."]
    pub url: ::std::string::String,
}
impl McpServerSubtype1 {
    pub fn builder() -> builder::McpServerSubtype1 {
        Default::default()
    }
}
#[doc = "Unique identifier for a message within a session."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Unique identifier for a message within a session.\","]
#[doc = "  \"type\": \"string\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(
    :: serde :: Deserialize,
    :: serde :: Serialize,
    Clone,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
)]
#[serde(transparent)]
pub struct MessageId(pub ::std::string::String);
impl ::std::ops::Deref for MessageId {
    type Target = ::std::string::String;
    fn deref(&self) -> &::std::string::String {
        &self.0
    }
}
impl ::std::convert::From<MessageId> for ::std::string::String {
    fn from(value: MessageId) -> Self {
        value.0
    }
}
impl ::std::convert::From<::std::string::String> for MessageId {
    fn from(value: ::std::string::String) -> Self {
        Self(value)
    }
}
impl ::std::str::FromStr for MessageId {
    type Err = ::std::convert::Infallible;
    fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
        Ok(Self(value.to_string()))
    }
}
impl ::std::fmt::Display for MessageId {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        self.0.fmt(f)
    }
}
#[doc = "Request parameters for creating a new session.\n\nSee protocol docs: [Creating a Session](https://agentclientprotocol.com/protocol/session-setup#creating-a-session)"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Request parameters for creating a new session.\\n\\nSee protocol docs: [Creating a Session](https://agentclientprotocol.com/protocol/session-setup#creating-a-session)\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"cwd\","]
#[doc = "    \"mcpServers\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"additionalDirectories\": {"]
#[doc = "      \"description\": \"Additional workspace roots for this session. Each path must be absolute.\\n\\nThese expand the session's filesystem scope without changing `cwd`, which\\nremains the base for relative paths. When omitted or empty, no\\nadditional roots are activated for the new session.\","]
#[doc = "      \"type\": \"array\","]
#[doc = "      \"items\": {"]
#[doc = "        \"type\": \"string\""]
#[doc = "      },"]
#[doc = "      \"x-deserialize-default-on-error\": true,"]
#[doc = "      \"x-deserialize-skip-invalid-items\": true"]
#[doc = "    },"]
#[doc = "    \"cwd\": {"]
#[doc = "      \"description\": \"The working directory for this session. Must be an absolute path.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"mcpServers\": {"]
#[doc = "      \"description\": \"List of MCP (Model Context Protocol) servers the agent should connect to.\","]
#[doc = "      \"type\": \"array\","]
#[doc = "      \"items\": {"]
#[doc = "        \"$ref\": \"#/$defs/McpServer\""]
#[doc = "      },"]
#[doc = "      \"x-deserialize-default-on-error\": true,"]
#[doc = "      \"x-deserialize-skip-invalid-items\": true"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"x-method\": \"session/new\","]
#[doc = "  \"x-side\": \"agent\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct NewSessionRequest {
    #[doc = "Additional workspace roots for this session. Each path must be absolute.\n\nThese expand the session's filesystem scope without changing `cwd`, which\nremains the base for relative paths. When omitted or empty, no\nadditional roots are activated for the new session."]
    #[serde(
        rename = "additionalDirectories",
        default,
        skip_serializing_if = "::std::vec::Vec::is_empty"
    )]
    pub additional_directories: ::std::vec::Vec<::std::string::String>,
    #[doc = "The working directory for this session. Must be an absolute path."]
    pub cwd: ::std::string::String,
    #[doc = "List of MCP (Model Context Protocol) servers the agent should connect to."]
    #[serde(rename = "mcpServers")]
    pub mcp_servers: ::std::vec::Vec<McpServer>,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
}
impl NewSessionRequest {
    pub fn builder() -> builder::NewSessionRequest {
        Default::default()
    }
}
#[doc = "Response from creating a new session.\n\nSee protocol docs: [Creating a Session](https://agentclientprotocol.com/protocol/session-setup#creating-a-session)"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Response from creating a new session.\\n\\nSee protocol docs: [Creating a Session](https://agentclientprotocol.com/protocol/session-setup#creating-a-session)\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"sessionId\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"configOptions\": {"]
#[doc = "      \"description\": \"Initial session configuration options if supported by the Agent.\","]
#[doc = "      \"type\": ["]
#[doc = "        \"array\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"items\": {"]
#[doc = "        \"$ref\": \"#/$defs/SessionConfigOption\""]
#[doc = "      },"]
#[doc = "      \"x-deserialize-default-on-error\": true,"]
#[doc = "      \"x-deserialize-skip-invalid-items\": true"]
#[doc = "    },"]
#[doc = "    \"modes\": {"]
#[doc = "      \"description\": \"Initial mode state if supported by the Agent\\n\\nSee protocol docs: [Session Modes](https://agentclientprotocol.com/protocol/session-modes)\","]
#[doc = "      \"anyOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/SessionModeState\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"type\": \"null\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"sessionId\": {"]
#[doc = "      \"description\": \"Unique identifier for the created session.\\n\\nUsed in all subsequent requests for this conversation.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/SessionId\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"x-method\": \"session/new\","]
#[doc = "  \"x-side\": \"agent\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct NewSessionResponse {
    #[doc = "Initial session configuration options if supported by the Agent."]
    #[serde(
        rename = "configOptions",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub config_options: ::std::option::Option<::std::vec::Vec<SessionConfigOption>>,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "Initial mode state if supported by the Agent\n\nSee protocol docs: [Session Modes](https://agentclientprotocol.com/protocol/session-modes)"]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub modes: ::std::option::Option<SessionModeState>,
    #[doc = "Unique identifier for the created session.\n\nUsed in all subsequent requests for this conversation."]
    #[serde(rename = "sessionId")]
    pub session_id: SessionId,
}
impl NewSessionResponse {
    pub fn builder() -> builder::NewSessionResponse {
        Default::default()
    }
}
#[doc = "An option presented to the user when requesting permission."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"An option presented to the user when requesting permission.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"kind\","]
#[doc = "    \"name\","]
#[doc = "    \"optionId\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"kind\": {"]
#[doc = "      \"description\": \"Hint about the nature of this permission option.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/PermissionOptionKind\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    \"name\": {"]
#[doc = "      \"description\": \"Human-readable label to display to the user.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"optionId\": {"]
#[doc = "      \"description\": \"Unique identifier for this permission option.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/PermissionOptionId\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct PermissionOption {
    #[doc = "Hint about the nature of this permission option."]
    pub kind: PermissionOptionKind,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "Human-readable label to display to the user."]
    pub name: ::std::string::String,
    #[doc = "Unique identifier for this permission option."]
    #[serde(rename = "optionId")]
    pub option_id: PermissionOptionId,
}
impl PermissionOption {
    pub fn builder() -> builder::PermissionOption {
        Default::default()
    }
}
#[doc = "Unique identifier for a permission option."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Unique identifier for a permission option.\","]
#[doc = "  \"type\": \"string\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(
    :: serde :: Deserialize,
    :: serde :: Serialize,
    Clone,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
)]
#[serde(transparent)]
pub struct PermissionOptionId(pub ::std::string::String);
impl ::std::ops::Deref for PermissionOptionId {
    type Target = ::std::string::String;
    fn deref(&self) -> &::std::string::String {
        &self.0
    }
}
impl ::std::convert::From<PermissionOptionId> for ::std::string::String {
    fn from(value: PermissionOptionId) -> Self {
        value.0
    }
}
impl ::std::convert::From<::std::string::String> for PermissionOptionId {
    fn from(value: ::std::string::String) -> Self {
        Self(value)
    }
}
impl ::std::str::FromStr for PermissionOptionId {
    type Err = ::std::convert::Infallible;
    fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
        Ok(Self(value.to_string()))
    }
}
impl ::std::fmt::Display for PermissionOptionId {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        self.0.fmt(f)
    }
}
#[doc = "The type of permission option being presented to the user.\n\nHelps clients choose appropriate icons and UI treatment."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"The type of permission option being presented to the user.\\n\\nHelps clients choose appropriate icons and UI treatment.\","]
#[doc = "  \"oneOf\": ["]
#[doc = "    {"]
#[doc = "      \"description\": \"Allow this operation only this time.\","]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"const\": \"allow_once\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"description\": \"Allow this operation and remember the choice.\","]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"const\": \"allow_always\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"description\": \"Reject this operation only this time.\","]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"const\": \"reject_once\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"description\": \"Reject this operation and remember the choice.\","]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"const\": \"reject_always\""]
#[doc = "    }"]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(
    :: serde :: Deserialize,
    :: serde :: Serialize,
    Clone,
    Copy,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
)]
pub enum PermissionOptionKind {
    #[doc = "Allow this operation only this time."]
    #[serde(rename = "allow_once")]
    AllowOnce,
    #[doc = "Allow this operation and remember the choice."]
    #[serde(rename = "allow_always")]
    AllowAlways,
    #[doc = "Reject this operation only this time."]
    #[serde(rename = "reject_once")]
    RejectOnce,
    #[doc = "Reject this operation and remember the choice."]
    #[serde(rename = "reject_always")]
    RejectAlways,
}
impl ::std::fmt::Display for PermissionOptionKind {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        match *self {
            Self::AllowOnce => f.write_str("allow_once"),
            Self::AllowAlways => f.write_str("allow_always"),
            Self::RejectOnce => f.write_str("reject_once"),
            Self::RejectAlways => f.write_str("reject_always"),
        }
    }
}
impl ::std::str::FromStr for PermissionOptionKind {
    type Err = self::error::ConversionError;
    fn from_str(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        match value {
            "allow_once" => Ok(Self::AllowOnce),
            "allow_always" => Ok(Self::AllowAlways),
            "reject_once" => Ok(Self::RejectOnce),
            "reject_always" => Ok(Self::RejectAlways),
            _ => Err("invalid value".into()),
        }
    }
}
impl ::std::convert::TryFrom<&str> for PermissionOptionKind {
    type Error = self::error::ConversionError;
    fn try_from(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<&::std::string::String> for PermissionOptionKind {
    type Error = self::error::ConversionError;
    fn try_from(
        value: &::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<::std::string::String> for PermissionOptionKind {
    type Error = self::error::ConversionError;
    fn try_from(
        value: ::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
#[doc = "An execution plan for accomplishing complex tasks.\n\nPlans consist of multiple entries representing individual tasks or goals.\nAgents report plans to clients to provide visibility into their execution strategy.\nPlans can evolve during execution as the agent discovers new requirements or completes tasks.\n\nSee protocol docs: [Agent Plan](https://agentclientprotocol.com/protocol/agent-plan)"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"An execution plan for accomplishing complex tasks.\\n\\nPlans consist of multiple entries representing individual tasks or goals.\\nAgents report plans to clients to provide visibility into their execution strategy.\\nPlans can evolve during execution as the agent discovers new requirements or completes tasks.\\n\\nSee protocol docs: [Agent Plan](https://agentclientprotocol.com/protocol/agent-plan)\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"entries\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"entries\": {"]
#[doc = "      \"description\": \"The list of tasks to be accomplished.\\n\\nWhen updating a plan, the agent must send a complete list of all entries\\nwith their current status. The client replaces the entire plan with each update.\","]
#[doc = "      \"type\": \"array\","]
#[doc = "      \"items\": {"]
#[doc = "        \"$ref\": \"#/$defs/PlanEntry\""]
#[doc = "      },"]
#[doc = "      \"x-deserialize-default-on-error\": true,"]
#[doc = "      \"x-deserialize-skip-invalid-items\": true"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct Plan {
    #[doc = "The list of tasks to be accomplished.\n\nWhen updating a plan, the agent must send a complete list of all entries\nwith their current status. The client replaces the entire plan with each update."]
    pub entries: ::std::vec::Vec<PlanEntry>,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
}
impl Plan {
    pub fn builder() -> builder::Plan {
        Default::default()
    }
}
#[doc = "A single entry in the execution plan.\n\nRepresents a task or goal that the assistant intends to accomplish\nas part of fulfilling the user's request.\nSee protocol docs: [Plan Entries](https://agentclientprotocol.com/protocol/agent-plan#plan-entries)"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"A single entry in the execution plan.\\n\\nRepresents a task or goal that the assistant intends to accomplish\\nas part of fulfilling the user's request.\\nSee protocol docs: [Plan Entries](https://agentclientprotocol.com/protocol/agent-plan#plan-entries)\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"content\","]
#[doc = "    \"priority\","]
#[doc = "    \"status\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"content\": {"]
#[doc = "      \"description\": \"Human-readable description of what this task aims to accomplish.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"priority\": {"]
#[doc = "      \"description\": \"The relative importance of this task.\\nUsed to indicate which tasks are most critical to the overall goal.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/PlanEntryPriority\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    \"status\": {"]
#[doc = "      \"description\": \"Current execution status of this task.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/PlanEntryStatus\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct PlanEntry {
    #[doc = "Human-readable description of what this task aims to accomplish."]
    pub content: ::std::string::String,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "The relative importance of this task.\nUsed to indicate which tasks are most critical to the overall goal."]
    pub priority: PlanEntryPriority,
    #[doc = "Current execution status of this task."]
    pub status: PlanEntryStatus,
}
impl PlanEntry {
    pub fn builder() -> builder::PlanEntry {
        Default::default()
    }
}
#[doc = "Priority levels for plan entries.\n\nUsed to indicate the relative importance or urgency of different\ntasks in the execution plan.\nSee protocol docs: [Plan Entries](https://agentclientprotocol.com/protocol/agent-plan#plan-entries)"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Priority levels for plan entries.\\n\\nUsed to indicate the relative importance or urgency of different\\ntasks in the execution plan.\\nSee protocol docs: [Plan Entries](https://agentclientprotocol.com/protocol/agent-plan#plan-entries)\","]
#[doc = "  \"oneOf\": ["]
#[doc = "    {"]
#[doc = "      \"description\": \"High priority task - critical to the overall goal.\","]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"const\": \"high\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"description\": \"Medium priority task - important but not critical.\","]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"const\": \"medium\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"description\": \"Low priority task - nice to have but not essential.\","]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"const\": \"low\""]
#[doc = "    }"]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(
    :: serde :: Deserialize,
    :: serde :: Serialize,
    Clone,
    Copy,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
)]
pub enum PlanEntryPriority {
    #[doc = "High priority task - critical to the overall goal."]
    #[serde(rename = "high")]
    High,
    #[doc = "Medium priority task - important but not critical."]
    #[serde(rename = "medium")]
    Medium,
    #[doc = "Low priority task - nice to have but not essential."]
    #[serde(rename = "low")]
    Low,
}
impl ::std::fmt::Display for PlanEntryPriority {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        match *self {
            Self::High => f.write_str("high"),
            Self::Medium => f.write_str("medium"),
            Self::Low => f.write_str("low"),
        }
    }
}
impl ::std::str::FromStr for PlanEntryPriority {
    type Err = self::error::ConversionError;
    fn from_str(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        match value {
            "high" => Ok(Self::High),
            "medium" => Ok(Self::Medium),
            "low" => Ok(Self::Low),
            _ => Err("invalid value".into()),
        }
    }
}
impl ::std::convert::TryFrom<&str> for PlanEntryPriority {
    type Error = self::error::ConversionError;
    fn try_from(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<&::std::string::String> for PlanEntryPriority {
    type Error = self::error::ConversionError;
    fn try_from(
        value: &::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<::std::string::String> for PlanEntryPriority {
    type Error = self::error::ConversionError;
    fn try_from(
        value: ::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
#[doc = "Status of a plan entry in the execution flow.\n\nTracks the lifecycle of each task from planning through completion.\nSee protocol docs: [Plan Entries](https://agentclientprotocol.com/protocol/agent-plan#plan-entries)"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Status of a plan entry in the execution flow.\\n\\nTracks the lifecycle of each task from planning through completion.\\nSee protocol docs: [Plan Entries](https://agentclientprotocol.com/protocol/agent-plan#plan-entries)\","]
#[doc = "  \"oneOf\": ["]
#[doc = "    {"]
#[doc = "      \"description\": \"The task has not started yet.\","]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"const\": \"pending\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"description\": \"The task is currently being worked on.\","]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"const\": \"in_progress\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"description\": \"The task has been successfully completed.\","]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"const\": \"completed\""]
#[doc = "    }"]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(
    :: serde :: Deserialize,
    :: serde :: Serialize,
    Clone,
    Copy,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
)]
pub enum PlanEntryStatus {
    #[doc = "The task has not started yet."]
    #[serde(rename = "pending")]
    Pending,
    #[doc = "The task is currently being worked on."]
    #[serde(rename = "in_progress")]
    InProgress,
    #[doc = "The task has been successfully completed."]
    #[serde(rename = "completed")]
    Completed,
}
impl ::std::fmt::Display for PlanEntryStatus {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        match *self {
            Self::Pending => f.write_str("pending"),
            Self::InProgress => f.write_str("in_progress"),
            Self::Completed => f.write_str("completed"),
        }
    }
}
impl ::std::str::FromStr for PlanEntryStatus {
    type Err = self::error::ConversionError;
    fn from_str(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        match value {
            "pending" => Ok(Self::Pending),
            "in_progress" => Ok(Self::InProgress),
            "completed" => Ok(Self::Completed),
            _ => Err("invalid value".into()),
        }
    }
}
impl ::std::convert::TryFrom<&str> for PlanEntryStatus {
    type Error = self::error::ConversionError;
    fn try_from(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<&::std::string::String> for PlanEntryStatus {
    type Error = self::error::ConversionError;
    fn try_from(
        value: &::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<::std::string::String> for PlanEntryStatus {
    type Error = self::error::ConversionError;
    fn try_from(
        value: ::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
#[doc = "Prompt capabilities supported by the agent in `session/prompt` requests.\n\nBaseline agent functionality requires support for [`ContentBlock::Text`]\nand [`ContentBlock::ResourceLink`] in prompt requests.\n\nOther variants must be explicitly opted in to.\nCapabilities for different types of content in prompt requests.\n\nIndicates which content types beyond the baseline (text and resource links)\nthe agent can process.\n\nSee protocol docs: [Prompt Capabilities](https://agentclientprotocol.com/protocol/initialization#prompt-capabilities)"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Prompt capabilities supported by the agent in `session/prompt` requests.\\n\\nBaseline agent functionality requires support for [`ContentBlock::Text`]\\nand [`ContentBlock::ResourceLink`] in prompt requests.\\n\\nOther variants must be explicitly opted in to.\\nCapabilities for different types of content in prompt requests.\\n\\nIndicates which content types beyond the baseline (text and resource links)\\nthe agent can process.\\n\\nSee protocol docs: [Prompt Capabilities](https://agentclientprotocol.com/protocol/initialization#prompt-capabilities)\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"audio\": {"]
#[doc = "      \"description\": \"Agent supports [`ContentBlock::Audio`].\","]
#[doc = "      \"default\": false,"]
#[doc = "      \"type\": \"boolean\","]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"embeddedContext\": {"]
#[doc = "      \"description\": \"Agent supports embedded context in `session/prompt` requests.\\n\\nWhen enabled, the Client is allowed to include [`ContentBlock::Resource`]\\nin prompt requests for pieces of context that are referenced in the message.\","]
#[doc = "      \"default\": false,"]
#[doc = "      \"type\": \"boolean\","]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"image\": {"]
#[doc = "      \"description\": \"Agent supports [`ContentBlock::Image`].\","]
#[doc = "      \"default\": false,"]
#[doc = "      \"type\": \"boolean\","]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct PromptCapabilities {
    #[doc = "Agent supports [`ContentBlock::Audio`]."]
    #[serde(default)]
    pub audio: bool,
    #[doc = "Agent supports embedded context in `session/prompt` requests.\n\nWhen enabled, the Client is allowed to include [`ContentBlock::Resource`]\nin prompt requests for pieces of context that are referenced in the message."]
    #[serde(rename = "embeddedContext", default)]
    pub embedded_context: bool,
    #[doc = "Agent supports [`ContentBlock::Image`]."]
    #[serde(default)]
    pub image: bool,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
}
impl ::std::default::Default for PromptCapabilities {
    fn default() -> Self {
        Self {
            audio: Default::default(),
            embedded_context: Default::default(),
            image: Default::default(),
            meta: Default::default(),
        }
    }
}
impl PromptCapabilities {
    pub fn builder() -> builder::PromptCapabilities {
        Default::default()
    }
}
#[doc = "Request parameters for sending a user prompt to the agent.\n\nContains the user's message and any additional context.\n\nSee protocol docs: [User Message](https://agentclientprotocol.com/protocol/prompt-turn#1-user-message)"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Request parameters for sending a user prompt to the agent.\\n\\nContains the user's message and any additional context.\\n\\nSee protocol docs: [User Message](https://agentclientprotocol.com/protocol/prompt-turn#1-user-message)\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"prompt\","]
#[doc = "    \"sessionId\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"prompt\": {"]
#[doc = "      \"description\": \"The blocks of content that compose the user's message.\\n\\nAs a baseline, the Agent MUST support [`ContentBlock::Text`] and [`ContentBlock::ResourceLink`],\\nwhile other variants are optionally enabled via [`PromptCapabilities`].\\n\\nThe Client MUST adapt its interface according to [`PromptCapabilities`].\\n\\nThe client MAY include referenced pieces of context as either\\n[`ContentBlock::Resource`] or [`ContentBlock::ResourceLink`].\\n\\nWhen available, [`ContentBlock::Resource`] is preferred\\nas it avoids extra round-trips and allows the message to include\\npieces of context from sources the agent may not have access to.\","]
#[doc = "      \"type\": \"array\","]
#[doc = "      \"items\": {"]
#[doc = "        \"$ref\": \"#/$defs/ContentBlock\""]
#[doc = "      }"]
#[doc = "    },"]
#[doc = "    \"sessionId\": {"]
#[doc = "      \"description\": \"The ID of the session to send this user message to\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/SessionId\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"x-method\": \"session/prompt\","]
#[doc = "  \"x-side\": \"agent\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct PromptRequest {
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "The blocks of content that compose the user's message.\n\nAs a baseline, the Agent MUST support [`ContentBlock::Text`] and [`ContentBlock::ResourceLink`],\nwhile other variants are optionally enabled via [`PromptCapabilities`].\n\nThe Client MUST adapt its interface according to [`PromptCapabilities`].\n\nThe client MAY include referenced pieces of context as either\n[`ContentBlock::Resource`] or [`ContentBlock::ResourceLink`].\n\nWhen available, [`ContentBlock::Resource`] is preferred\nas it avoids extra round-trips and allows the message to include\npieces of context from sources the agent may not have access to."]
    pub prompt: ::std::vec::Vec<ContentBlock>,
    #[doc = "The ID of the session to send this user message to"]
    #[serde(rename = "sessionId")]
    pub session_id: SessionId,
}
impl PromptRequest {
    pub fn builder() -> builder::PromptRequest {
        Default::default()
    }
}
#[doc = "Response from processing a user prompt.\n\nSee protocol docs: [Check for Completion](https://agentclientprotocol.com/protocol/prompt-turn#4-check-for-completion)"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Response from processing a user prompt.\\n\\nSee protocol docs: [Check for Completion](https://agentclientprotocol.com/protocol/prompt-turn#4-check-for-completion)\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"stopReason\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"stopReason\": {"]
#[doc = "      \"description\": \"Indicates why the agent stopped processing the turn.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/StopReason\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"x-method\": \"session/prompt\","]
#[doc = "  \"x-side\": \"agent\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct PromptResponse {
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "Indicates why the agent stopped processing the turn."]
    #[serde(rename = "stopReason")]
    pub stop_reason: StopReason,
}
impl PromptResponse {
    pub fn builder() -> builder::PromptResponse {
        Default::default()
    }
}
#[doc = "A message (request, response, or notification) with `\"jsonrpc\": \"2.0\"` specified as\n[required by JSON-RPC 2.0 Specification][1].\n\n[1]: https://www.jsonrpc.org/specification#compatibility"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"ProtocolLevel\","]
#[doc = "  \"description\": \"A message (request, response, or notification) with `\\\"jsonrpc\\\": \\\"2.0\\\"` specified as\\n[required by JSON-RPC 2.0 Specification][1].\\n\\n[1]: https://www.jsonrpc.org/specification#compatibility\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"jsonrpc\","]
#[doc = "    \"method\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"jsonrpc\": {"]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"enum\": ["]
#[doc = "        \"2.0\""]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    \"method\": {"]
#[doc = "      \"description\": \"The notification method name.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"params\": {"]
#[doc = "      \"description\": \"Method-specific notification parameters.\","]
#[doc = "      \"anyOf\": ["]
#[doc = "        {"]
#[doc = "          \"description\": \"General protocol-level notifications that all sides are expected to\\nimplement.\\n\\nNotifications whose methods start with '$/' are messages which\\nare protocol implementation dependent and might not be implementable in all\\nclients or agents. For example if the implementation uses a single threaded\\nsynchronous programming language then there is little it can do to react to\\na `$/cancel_request` notification. If an agent or client receives\\nnotifications starting with '$/' it is free to ignore the notification.\\n\\nNotifications do not expect a response.\","]
#[doc = "          \"anyOf\": ["]
#[doc = "            {"]
#[doc = "              \"title\": \"CancelRequestNotification\","]
#[doc = "              \"description\": \"Cancels an ongoing request.\\n\\nThis is a notification sent by the side that sent a request to cancel that request.\\n\\nUpon receiving this notification, the receiver:\\n\\n1. MAY cancel the corresponding request activity and all nested activities\\n2. MAY send any pending notifications.\\n3. MUST send one of these responses for the original request:\\n  - Valid response with appropriate data (partial results or cancellation marker)\\n  - Error response with code `-32800` (Cancelled)\\n\\nSee protocol docs: [Cancellation](https://agentclientprotocol.com/protocol/cancellation)\","]
#[doc = "              \"allOf\": ["]
#[doc = "                {"]
#[doc = "                  \"$ref\": \"#/$defs/CancelRequestNotification\""]
#[doc = "                }"]
#[doc = "              ]"]
#[doc = "            }"]
#[doc = "          ]"]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"type\": \"null\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"x-docs-ignore\": true"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct ProtocolLevel {
    pub jsonrpc: ProtocolLevelJsonrpc,
    #[doc = "The notification method name."]
    pub method: ::std::string::String,
    #[doc = "Method-specific notification parameters."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub params: ::std::option::Option<CancelRequestNotification>,
}
impl ProtocolLevel {
    pub fn builder() -> builder::ProtocolLevel {
        Default::default()
    }
}
#[doc = "`ProtocolLevelJsonrpc`"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"type\": \"string\","]
#[doc = "  \"enum\": ["]
#[doc = "    \"2.0\""]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(
    :: serde :: Deserialize,
    :: serde :: Serialize,
    Clone,
    Copy,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
)]
pub enum ProtocolLevelJsonrpc {
    #[serde(rename = "2.0")]
    X20,
}
impl ::std::fmt::Display for ProtocolLevelJsonrpc {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        match *self {
            Self::X20 => f.write_str("2.0"),
        }
    }
}
impl ::std::str::FromStr for ProtocolLevelJsonrpc {
    type Err = self::error::ConversionError;
    fn from_str(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        match value {
            "2.0" => Ok(Self::X20),
            _ => Err("invalid value".into()),
        }
    }
}
impl ::std::convert::TryFrom<&str> for ProtocolLevelJsonrpc {
    type Error = self::error::ConversionError;
    fn try_from(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<&::std::string::String> for ProtocolLevelJsonrpc {
    type Error = self::error::ConversionError;
    fn try_from(
        value: &::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<::std::string::String> for ProtocolLevelJsonrpc {
    type Error = self::error::ConversionError;
    fn try_from(
        value: ::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
#[doc = "Protocol version identifier.\n\nThis version is only bumped for breaking changes.\nNon-breaking changes should be introduced via capabilities."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Protocol version identifier.\\n\\nThis version is only bumped for breaking changes.\\nNon-breaking changes should be introduced via capabilities.\","]
#[doc = "  \"type\": \"integer\","]
#[doc = "  \"format\": \"uint16\","]
#[doc = "  \"maximum\": 65535.0,"]
#[doc = "  \"minimum\": 0.0"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
#[serde(transparent)]
pub struct ProtocolVersion(pub u16);
impl ::std::ops::Deref for ProtocolVersion {
    type Target = u16;
    fn deref(&self) -> &u16 {
        &self.0
    }
}
impl ::std::convert::From<ProtocolVersion> for u16 {
    fn from(value: ProtocolVersion) -> Self {
        value.0
    }
}
impl ::std::convert::From<u16> for ProtocolVersion {
    fn from(value: u16) -> Self {
        Self(value)
    }
}
impl ::std::str::FromStr for ProtocolVersion {
    type Err = <u16 as ::std::str::FromStr>::Err;
    fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
        Ok(Self(value.parse()?))
    }
}
impl ::std::convert::TryFrom<&str> for ProtocolVersion {
    type Error = <u16 as ::std::str::FromStr>::Err;
    fn try_from(value: &str) -> ::std::result::Result<Self, Self::Error> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<String> for ProtocolVersion {
    type Error = <u16 as ::std::str::FromStr>::Err;
    fn try_from(value: String) -> ::std::result::Result<Self, Self::Error> {
        value.parse()
    }
}
impl ::std::fmt::Display for ProtocolVersion {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        self.0.fmt(f)
    }
}
#[doc = "Request to read content from a text file.\n\nOnly available if the client supports the `fs.readTextFile` capability."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Request to read content from a text file.\\n\\nOnly available if the client supports the `fs.readTextFile` capability.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"path\","]
#[doc = "    \"sessionId\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"limit\": {"]
#[doc = "      \"description\": \"Maximum number of lines to read.\","]
#[doc = "      \"type\": ["]
#[doc = "        \"integer\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"format\": \"uint32\","]
#[doc = "      \"minimum\": 0.0,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"line\": {"]
#[doc = "      \"description\": \"Line number to start reading from (1-based).\","]
#[doc = "      \"type\": ["]
#[doc = "        \"integer\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"format\": \"uint32\","]
#[doc = "      \"minimum\": 0.0,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"path\": {"]
#[doc = "      \"description\": \"Absolute path to the file to read.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"sessionId\": {"]
#[doc = "      \"description\": \"The session ID for this request.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/SessionId\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"x-method\": \"fs/read_text_file\","]
#[doc = "  \"x-side\": \"client\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct ReadTextFileRequest {
    #[doc = "Maximum number of lines to read."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub limit: ::std::option::Option<u32>,
    #[doc = "Line number to start reading from (1-based)."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub line: ::std::option::Option<u32>,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "Absolute path to the file to read."]
    pub path: ::std::string::String,
    #[doc = "The session ID for this request."]
    #[serde(rename = "sessionId")]
    pub session_id: SessionId,
}
impl ReadTextFileRequest {
    pub fn builder() -> builder::ReadTextFileRequest {
        Default::default()
    }
}
#[doc = "Response containing the contents of a text file."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Response containing the contents of a text file.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"content\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"content\": {"]
#[doc = "      \"description\": \"Content payload returned by this response.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"x-method\": \"fs/read_text_file\","]
#[doc = "  \"x-side\": \"client\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct ReadTextFileResponse {
    #[doc = "Content payload returned by this response."]
    pub content: ::std::string::String,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
}
impl ReadTextFileResponse {
    pub fn builder() -> builder::ReadTextFileResponse {
        Default::default()
    }
}
#[doc = "Request to release a terminal and free its resources."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Request to release a terminal and free its resources.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"sessionId\","]
#[doc = "    \"terminalId\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"sessionId\": {"]
#[doc = "      \"description\": \"The session ID for this request.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/SessionId\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    \"terminalId\": {"]
#[doc = "      \"description\": \"The ID of the terminal to release.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/TerminalId\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"x-method\": \"terminal/release\","]
#[doc = "  \"x-side\": \"client\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct ReleaseTerminalRequest {
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "The session ID for this request."]
    #[serde(rename = "sessionId")]
    pub session_id: SessionId,
    #[doc = "The ID of the terminal to release."]
    #[serde(rename = "terminalId")]
    pub terminal_id: TerminalId,
}
impl ReleaseTerminalRequest {
    pub fn builder() -> builder::ReleaseTerminalRequest {
        Default::default()
    }
}
#[doc = "Response to terminal/release method"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Response to terminal/release method\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"x-method\": \"terminal/release\","]
#[doc = "  \"x-side\": \"client\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct ReleaseTerminalResponse {
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
}
impl ::std::default::Default for ReleaseTerminalResponse {
    fn default() -> Self {
        Self {
            meta: Default::default(),
        }
    }
}
impl ReleaseTerminalResponse {
    pub fn builder() -> builder::ReleaseTerminalResponse {
        Default::default()
    }
}
#[doc = "JSON RPC Request Id\n\nAn identifier established by the Client that MUST contain a String, Number, or NULL value if included. If it is not included it is assumed to be a notification. The value SHOULD normally not be Null \\[1\\] and Numbers SHOULD NOT contain fractional parts \\[2\\]\n\nThe Server MUST reply with the same value in the Response object if included. This member is used to correlate the context between the two objects.\n\n\\[1\\] The use of Null as a value for the id member in a Request object is discouraged, because this specification uses a value of Null for Responses with an unknown id. Also, because JSON-RPC 1.0 uses an id value of Null for Notifications this could cause confusion in handling.\n\n\\[2\\] Fractional parts may be problematic, since many decimal fractions cannot be represented exactly as binary fractions."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"JSON RPC Request Id\\n\\nAn identifier established by the Client that MUST contain a String, Number, or NULL value if included. If it is not included it is assumed to be a notification. The value SHOULD normally not be Null \\\\[1\\\\] and Numbers SHOULD NOT contain fractional parts \\\\[2\\\\]\\n\\nThe Server MUST reply with the same value in the Response object if included. This member is used to correlate the context between the two objects.\\n\\n\\\\[1\\\\] The use of Null as a value for the id member in a Request object is discouraged, because this specification uses a value of Null for Responses with an unknown id. Also, because JSON-RPC 1.0 uses an id value of Null for Notifications this could cause confusion in handling.\\n\\n\\\\[2\\\\] Fractional parts may be problematic, since many decimal fractions cannot be represented exactly as binary fractions.\","]
#[doc = "  \"anyOf\": ["]
#[doc = "    {"]
#[doc = "      \"title\": \"Null\","]
#[doc = "      \"description\": \"The JSON-RPC `null` request id.\","]
#[doc = "      \"type\": \"null\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"Number\","]
#[doc = "      \"description\": \"A numeric JSON-RPC request id.\","]
#[doc = "      \"type\": \"integer\","]
#[doc = "      \"format\": \"int64\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"Str\","]
#[doc = "      \"description\": \"A string JSON-RPC request id.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    }"]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
#[serde(untagged)]
pub enum RequestId {
    Null,
    Number(i64),
    Str(::std::string::String),
}
impl ::std::convert::From<i64> for RequestId {
    fn from(value: i64) -> Self {
        Self::Number(value)
    }
}
#[doc = "The outcome of a permission request."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"The outcome of a permission request.\","]
#[doc = "  \"oneOf\": ["]
#[doc = "    {"]
#[doc = "      \"description\": \"The prompt turn was cancelled before the user responded.\\n\\nWhen a client sends a `session/cancel` notification to cancel an ongoing\\nprompt turn, it MUST respond to all pending `session/request_permission`\\nrequests with this `Cancelled` outcome.\\n\\nSee protocol docs: [Cancellation](https://agentclientprotocol.com/protocol/prompt-turn#cancellation)\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"required\": ["]
#[doc = "        \"outcome\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"outcome\": {"]
#[doc = "          \"type\": \"string\","]
#[doc = "          \"const\": \"cancelled\""]
#[doc = "        }"]
#[doc = "      }"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"description\": \"The user selected one of the provided options.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/SelectedPermissionOutcome\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"required\": ["]
#[doc = "        \"outcome\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"outcome\": {"]
#[doc = "          \"type\": \"string\","]
#[doc = "          \"const\": \"selected\""]
#[doc = "        }"]
#[doc = "      }"]
#[doc = "    }"]
#[doc = "  ],"]
#[doc = "  \"discriminator\": {"]
#[doc = "    \"propertyName\": \"outcome\""]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
#[serde(untagged)]
pub enum RequestPermissionOutcome {
    Variant0 {
        outcome: ::std::string::String,
    },
    Variant1 {
        #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
        #[serde(
            rename = "_meta",
            default,
            skip_serializing_if = "::std::option::Option::is_none"
        )]
        meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
        #[doc = "The ID of the option the user selected."]
        #[serde(rename = "optionId")]
        option_id: PermissionOptionId,
        outcome: ::std::string::String,
    },
}
#[doc = "Request for user permission to execute a tool call.\n\nSent when the agent needs authorization before performing a sensitive operation.\n\nSee protocol docs: [Requesting Permission](https://agentclientprotocol.com/protocol/tool-calls#requesting-permission)"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Request for user permission to execute a tool call.\\n\\nSent when the agent needs authorization before performing a sensitive operation.\\n\\nSee protocol docs: [Requesting Permission](https://agentclientprotocol.com/protocol/tool-calls#requesting-permission)\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"options\","]
#[doc = "    \"sessionId\","]
#[doc = "    \"toolCall\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"options\": {"]
#[doc = "      \"description\": \"Available permission options for the user to choose from.\","]
#[doc = "      \"type\": \"array\","]
#[doc = "      \"items\": {"]
#[doc = "        \"$ref\": \"#/$defs/PermissionOption\""]
#[doc = "      }"]
#[doc = "    },"]
#[doc = "    \"sessionId\": {"]
#[doc = "      \"description\": \"The session ID for this request.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/SessionId\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    \"toolCall\": {"]
#[doc = "      \"description\": \"Details about the tool call requiring permission.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/ToolCallUpdate\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"x-method\": \"session/request_permission\","]
#[doc = "  \"x-side\": \"client\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct RequestPermissionRequest {
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "Available permission options for the user to choose from."]
    pub options: ::std::vec::Vec<PermissionOption>,
    #[doc = "The session ID for this request."]
    #[serde(rename = "sessionId")]
    pub session_id: SessionId,
    #[doc = "Details about the tool call requiring permission."]
    #[serde(rename = "toolCall")]
    pub tool_call: ToolCallUpdate,
}
impl RequestPermissionRequest {
    pub fn builder() -> builder::RequestPermissionRequest {
        Default::default()
    }
}
#[doc = "Response to a permission request."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Response to a permission request.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"outcome\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"outcome\": {"]
#[doc = "      \"description\": \"The user's decision on the permission request.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/RequestPermissionOutcome\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"x-method\": \"session/request_permission\","]
#[doc = "  \"x-side\": \"client\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct RequestPermissionResponse {
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "The user's decision on the permission request."]
    pub outcome: RequestPermissionOutcome,
}
impl RequestPermissionResponse {
    pub fn builder() -> builder::RequestPermissionResponse {
        Default::default()
    }
}
#[doc = "A resource that the server is capable of reading, included in a prompt or tool call result."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"A resource that the server is capable of reading, included in a prompt or tool call result.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"name\","]
#[doc = "    \"uri\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"annotations\": {"]
#[doc = "      \"description\": \"Optional annotations that help clients decide how to display or route this content.\","]
#[doc = "      \"anyOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/Annotations\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"type\": \"null\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"description\": {"]
#[doc = "      \"description\": \"Optional human-readable details shown with this protocol object.\","]
#[doc = "      \"type\": ["]
#[doc = "        \"string\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"mimeType\": {"]
#[doc = "      \"description\": \"MIME type describing the encoded media payload.\","]
#[doc = "      \"type\": ["]
#[doc = "        \"string\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"name\": {"]
#[doc = "      \"description\": \"Human-readable name shown for this protocol object.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"size\": {"]
#[doc = "      \"description\": \"Optional size of the linked resource in bytes, if known.\","]
#[doc = "      \"type\": ["]
#[doc = "        \"integer\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"format\": \"int64\","]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"title\": {"]
#[doc = "      \"description\": \"Optional display title for end-user UI.\","]
#[doc = "      \"type\": ["]
#[doc = "        \"string\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"uri\": {"]
#[doc = "      \"description\": \"URI associated with this resource or media payload.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct ResourceLink {
    #[doc = "Optional annotations that help clients decide how to display or route this content."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub annotations: ::std::option::Option<Annotations>,
    #[doc = "Optional human-readable details shown with this protocol object."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub description: ::std::option::Option<::std::string::String>,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "MIME type describing the encoded media payload."]
    #[serde(
        rename = "mimeType",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub mime_type: ::std::option::Option<::std::string::String>,
    #[doc = "Human-readable name shown for this protocol object."]
    pub name: ::std::string::String,
    #[doc = "Optional size of the linked resource in bytes, if known."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub size: ::std::option::Option<i64>,
    #[doc = "Optional display title for end-user UI."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub title: ::std::option::Option<::std::string::String>,
    #[doc = "URI associated with this resource or media payload."]
    pub uri: ::std::string::String,
}
impl ResourceLink {
    pub fn builder() -> builder::ResourceLink {
        Default::default()
    }
}
#[doc = "Method-specific response data."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Method-specific response data.\","]
#[doc = "  \"anyOf\": ["]
#[doc = "    {"]
#[doc = "      \"title\": \"InitializeResponse\","]
#[doc = "      \"description\": \"Successful result returned for a `initialize` request.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/InitializeResponse\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"AuthenticateResponse\","]
#[doc = "      \"description\": \"Successful result returned for a `authenticate` request.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/AuthenticateResponse\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"LogoutResponse\","]
#[doc = "      \"description\": \"Successful result returned for a `logout` request.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/LogoutResponse\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"NewSessionResponse\","]
#[doc = "      \"description\": \"Successful result returned for a `session/new` request.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/NewSessionResponse\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"LoadSessionResponse\","]
#[doc = "      \"description\": \"Successful result returned for a `session/load` request.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/LoadSessionResponse\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"ListSessionsResponse\","]
#[doc = "      \"description\": \"Successful result returned for a `session/list` request.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/ListSessionsResponse\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"DeleteSessionResponse\","]
#[doc = "      \"description\": \"Successful result returned for a `session/delete` request.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/DeleteSessionResponse\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"ResumeSessionResponse\","]
#[doc = "      \"description\": \"Successful result returned for a `session/resume` request.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/ResumeSessionResponse\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"CloseSessionResponse\","]
#[doc = "      \"description\": \"Successful result returned for a `session/close` request.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/CloseSessionResponse\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"SetSessionModeResponse\","]
#[doc = "      \"description\": \"Successful result returned for a `session/set_mode` request.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/SetSessionModeResponse\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"SetSessionConfigOptionResponse\","]
#[doc = "      \"description\": \"Successful result returned for a `session/set_config_option` request.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/SetSessionConfigOptionResponse\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"PromptResponse\","]
#[doc = "      \"description\": \"Successful result returned for a `session/prompt` request.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/PromptResponse\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"ExtMethodResponse\","]
#[doc = "      \"description\": \"Successful result returned by an extension method outside the core ACP method set.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/ExtResponse\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct ResultResult {
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_0: ::std::option::Option<InitializeResponse>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_1: ::std::option::Option<AuthenticateResponse>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_2: ::std::option::Option<LogoutResponse>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_3: ::std::option::Option<NewSessionResponse>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_4: ::std::option::Option<LoadSessionResponse>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_5: ::std::option::Option<ListSessionsResponse>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_6: ::std::option::Option<DeleteSessionResponse>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_7: ::std::option::Option<ResumeSessionResponse>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_8: ::std::option::Option<CloseSessionResponse>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_9: ::std::option::Option<SetSessionModeResponse>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_10: ::std::option::Option<SetSessionConfigOptionResponse>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_11: ::std::option::Option<PromptResponse>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_12: ::std::option::Option<ExtResponse>,
}
impl ::std::default::Default for ResultResult {
    fn default() -> Self {
        Self {
            subtype_0: Default::default(),
            subtype_1: Default::default(),
            subtype_2: Default::default(),
            subtype_3: Default::default(),
            subtype_4: Default::default(),
            subtype_5: Default::default(),
            subtype_6: Default::default(),
            subtype_7: Default::default(),
            subtype_8: Default::default(),
            subtype_9: Default::default(),
            subtype_10: Default::default(),
            subtype_11: Default::default(),
            subtype_12: Default::default(),
        }
    }
}
impl ResultResult {
    pub fn builder() -> builder::ResultResult {
        Default::default()
    }
}
#[doc = "Request parameters for resuming an existing session.\n\nResumes an existing session without returning previous messages (unlike `session/load`).\nThis is useful for agents that can resume sessions but don't implement full session loading.\n\nOnly available if the Agent supports the `sessionCapabilities.resume` capability."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Request parameters for resuming an existing session.\\n\\nResumes an existing session without returning previous messages (unlike `session/load`).\\nThis is useful for agents that can resume sessions but don't implement full session loading.\\n\\nOnly available if the Agent supports the `sessionCapabilities.resume` capability.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"cwd\","]
#[doc = "    \"sessionId\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"additionalDirectories\": {"]
#[doc = "      \"description\": \"Additional workspace roots to activate for this session. Each path must be absolute.\\n\\nWhen omitted or empty, no additional roots are activated. When non-empty,\\nthis is the complete resulting additional-root list for the resumed\\nsession. It may differ from any previously used or reported list as long as\\nthe request `cwd` matches the session's `cwd`.\","]
#[doc = "      \"type\": \"array\","]
#[doc = "      \"items\": {"]
#[doc = "        \"type\": \"string\""]
#[doc = "      },"]
#[doc = "      \"x-deserialize-default-on-error\": true,"]
#[doc = "      \"x-deserialize-skip-invalid-items\": true"]
#[doc = "    },"]
#[doc = "    \"cwd\": {"]
#[doc = "      \"description\": \"The working directory for this session. Must be an absolute path.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"mcpServers\": {"]
#[doc = "      \"description\": \"List of MCP servers to connect to for this session.\","]
#[doc = "      \"type\": \"array\","]
#[doc = "      \"items\": {"]
#[doc = "        \"$ref\": \"#/$defs/McpServer\""]
#[doc = "      },"]
#[doc = "      \"x-deserialize-default-on-error\": true,"]
#[doc = "      \"x-deserialize-skip-invalid-items\": true"]
#[doc = "    },"]
#[doc = "    \"sessionId\": {"]
#[doc = "      \"description\": \"The ID of the session to resume.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/SessionId\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"x-method\": \"session/resume\","]
#[doc = "  \"x-side\": \"agent\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct ResumeSessionRequest {
    #[doc = "Additional workspace roots to activate for this session. Each path must be absolute.\n\nWhen omitted or empty, no additional roots are activated. When non-empty,\nthis is the complete resulting additional-root list for the resumed\nsession. It may differ from any previously used or reported list as long as\nthe request `cwd` matches the session's `cwd`."]
    #[serde(
        rename = "additionalDirectories",
        default,
        skip_serializing_if = "::std::vec::Vec::is_empty"
    )]
    pub additional_directories: ::std::vec::Vec<::std::string::String>,
    #[doc = "The working directory for this session. Must be an absolute path."]
    pub cwd: ::std::string::String,
    #[doc = "List of MCP servers to connect to for this session."]
    #[serde(
        rename = "mcpServers",
        default,
        skip_serializing_if = "::std::vec::Vec::is_empty"
    )]
    pub mcp_servers: ::std::vec::Vec<McpServer>,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "The ID of the session to resume."]
    #[serde(rename = "sessionId")]
    pub session_id: SessionId,
}
impl ResumeSessionRequest {
    pub fn builder() -> builder::ResumeSessionRequest {
        Default::default()
    }
}
#[doc = "Response from resuming an existing session."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Response from resuming an existing session.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"configOptions\": {"]
#[doc = "      \"description\": \"Initial session configuration options if supported by the Agent.\","]
#[doc = "      \"type\": ["]
#[doc = "        \"array\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"items\": {"]
#[doc = "        \"$ref\": \"#/$defs/SessionConfigOption\""]
#[doc = "      },"]
#[doc = "      \"x-deserialize-default-on-error\": true,"]
#[doc = "      \"x-deserialize-skip-invalid-items\": true"]
#[doc = "    },"]
#[doc = "    \"modes\": {"]
#[doc = "      \"description\": \"Initial mode state if supported by the Agent\\n\\nSee protocol docs: [Session Modes](https://agentclientprotocol.com/protocol/session-modes)\","]
#[doc = "      \"anyOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/SessionModeState\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"type\": \"null\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"x-method\": \"session/resume\","]
#[doc = "  \"x-side\": \"agent\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct ResumeSessionResponse {
    #[doc = "Initial session configuration options if supported by the Agent."]
    #[serde(
        rename = "configOptions",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub config_options: ::std::option::Option<::std::vec::Vec<SessionConfigOption>>,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "Initial mode state if supported by the Agent\n\nSee protocol docs: [Session Modes](https://agentclientprotocol.com/protocol/session-modes)"]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub modes: ::std::option::Option<SessionModeState>,
}
impl ::std::default::Default for ResumeSessionResponse {
    fn default() -> Self {
        Self {
            config_options: Default::default(),
            meta: Default::default(),
            modes: Default::default(),
        }
    }
}
impl ResumeSessionResponse {
    pub fn builder() -> builder::ResumeSessionResponse {
        Default::default()
    }
}
#[doc = "The sender or recipient of messages and data in a conversation."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"The sender or recipient of messages and data in a conversation.\","]
#[doc = "  \"oneOf\": ["]
#[doc = "    {"]
#[doc = "      \"description\": \"The assistant side of a conversation.\","]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"const\": \"assistant\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"description\": \"The user side of a conversation.\","]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"const\": \"user\""]
#[doc = "    }"]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(
    :: serde :: Deserialize,
    :: serde :: Serialize,
    Clone,
    Copy,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
)]
pub enum Role {
    #[doc = "The assistant side of a conversation."]
    #[serde(rename = "assistant")]
    Assistant,
    #[doc = "The user side of a conversation."]
    #[serde(rename = "user")]
    User,
}
impl ::std::fmt::Display for Role {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        match *self {
            Self::Assistant => f.write_str("assistant"),
            Self::User => f.write_str("user"),
        }
    }
}
impl ::std::str::FromStr for Role {
    type Err = self::error::ConversionError;
    fn from_str(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        match value {
            "assistant" => Ok(Self::Assistant),
            "user" => Ok(Self::User),
            _ => Err("invalid value".into()),
        }
    }
}
impl ::std::convert::TryFrom<&str> for Role {
    type Error = self::error::ConversionError;
    fn try_from(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<&::std::string::String> for Role {
    type Error = self::error::ConversionError;
    fn try_from(
        value: &::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<::std::string::String> for Role {
    type Error = self::error::ConversionError;
    fn try_from(
        value: ::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
#[doc = "The user selected one of the provided options."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"The user selected one of the provided options.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"optionId\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"optionId\": {"]
#[doc = "      \"description\": \"The ID of the option the user selected.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/PermissionOptionId\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct SelectedPermissionOutcome {
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "The ID of the option the user selected."]
    #[serde(rename = "optionId")]
    pub option_id: PermissionOptionId,
}
impl SelectedPermissionOutcome {
    pub fn builder() -> builder::SelectedPermissionOutcome {
        Default::default()
    }
}
#[doc = "Capabilities for additional session directories support.\n\nSupplying `{}` means the agent supports the `additionalDirectories` field on\nsupported session lifecycle requests. Agents that also support\n`session/list` may return `SessionInfo.additionalDirectories` to report the\ncomplete ordered additional-root list associated with a listed session."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Capabilities for additional session directories support.\\n\\nSupplying `{}` means the agent supports the `additionalDirectories` field on\\nsupported session lifecycle requests. Agents that also support\\n`session/list` may return `SessionInfo.additionalDirectories` to report the\\ncomplete ordered additional-root list associated with a listed session.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct SessionAdditionalDirectoriesCapabilities {
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
}
impl ::std::default::Default for SessionAdditionalDirectoriesCapabilities {
    fn default() -> Self {
        Self {
            meta: Default::default(),
        }
    }
}
impl SessionAdditionalDirectoriesCapabilities {
    pub fn builder() -> builder::SessionAdditionalDirectoriesCapabilities {
        Default::default()
    }
}
#[doc = "Session capabilities supported by the agent.\n\nAs a baseline, all Agents **MUST** support `session/new`, `session/prompt`, `session/cancel`, and `session/update`.\n\nOptionally, they **MAY** support other session methods and notifications by specifying additional capabilities.\n\nNote: `session/load` is still handled by the top-level `load_session` capability. This will be unified in future versions of the protocol.\n\nSee protocol docs: [Session Capabilities](https://agentclientprotocol.com/protocol/initialization#session-capabilities)"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Session capabilities supported by the agent.\\n\\nAs a baseline, all Agents **MUST** support `session/new`, `session/prompt`, `session/cancel`, and `session/update`.\\n\\nOptionally, they **MAY** support other session methods and notifications by specifying additional capabilities.\\n\\nNote: `session/load` is still handled by the top-level `load_session` capability. This will be unified in future versions of the protocol.\\n\\nSee protocol docs: [Session Capabilities](https://agentclientprotocol.com/protocol/initialization#session-capabilities)\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"additionalDirectories\": {"]
#[doc = "      \"description\": \"Whether the agent supports `additionalDirectories` on supported session lifecycle requests.\\n\\nOptional. Omitted or `null` both mean the agent does not advertise support.\\nSupplying `{}` means the agent supports `additionalDirectories` on\\nsupported session lifecycle requests.\\n\\nAgents that also support `session/list` may return\\n`SessionInfo.additionalDirectories` to report the complete ordered\\nadditional-root list associated with a listed session.\","]
#[doc = "      \"anyOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/SessionAdditionalDirectoriesCapabilities\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"type\": \"null\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"close\": {"]
#[doc = "      \"description\": \"Whether the agent supports `session/close`.\\n\\nOptional. Omitted or `null` both mean the agent does not advertise support.\\nSupplying `{}` means the agent supports closing sessions.\","]
#[doc = "      \"anyOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/SessionCloseCapabilities\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"type\": \"null\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"delete\": {"]
#[doc = "      \"description\": \"Whether the agent supports `session/delete`.\\n\\nOptional. Omitted or `null` both mean the agent does not advertise support.\\nSupplying `{}` means the agent supports deleting sessions from `session/list`.\","]
#[doc = "      \"anyOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/SessionDeleteCapabilities\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"type\": \"null\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"list\": {"]
#[doc = "      \"description\": \"Whether the agent supports `session/list`.\\n\\nOptional. Omitted or `null` both mean the agent does not advertise support.\\nSupplying `{}` means the agent supports listing sessions.\","]
#[doc = "      \"anyOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/SessionListCapabilities\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"type\": \"null\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"resume\": {"]
#[doc = "      \"description\": \"Whether the agent supports `session/resume`.\\n\\nOptional. Omitted or `null` both mean the agent does not advertise support.\\nSupplying `{}` means the agent supports resuming sessions.\","]
#[doc = "      \"anyOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/SessionResumeCapabilities\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"type\": \"null\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct SessionCapabilities {
    #[doc = "Whether the agent supports `additionalDirectories` on supported session lifecycle requests.\n\nOptional. Omitted or `null` both mean the agent does not advertise support.\nSupplying `{}` means the agent supports `additionalDirectories` on\nsupported session lifecycle requests.\n\nAgents that also support `session/list` may return\n`SessionInfo.additionalDirectories` to report the complete ordered\nadditional-root list associated with a listed session."]
    #[serde(
        rename = "additionalDirectories",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub additional_directories: ::std::option::Option<SessionAdditionalDirectoriesCapabilities>,
    #[doc = "Whether the agent supports `session/close`.\n\nOptional. Omitted or `null` both mean the agent does not advertise support.\nSupplying `{}` means the agent supports closing sessions."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub close: ::std::option::Option<SessionCloseCapabilities>,
    #[doc = "Whether the agent supports `session/delete`.\n\nOptional. Omitted or `null` both mean the agent does not advertise support.\nSupplying `{}` means the agent supports deleting sessions from `session/list`."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub delete: ::std::option::Option<SessionDeleteCapabilities>,
    #[doc = "Whether the agent supports `session/list`.\n\nOptional. Omitted or `null` both mean the agent does not advertise support.\nSupplying `{}` means the agent supports listing sessions."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub list: ::std::option::Option<SessionListCapabilities>,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "Whether the agent supports `session/resume`.\n\nOptional. Omitted or `null` both mean the agent does not advertise support.\nSupplying `{}` means the agent supports resuming sessions."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub resume: ::std::option::Option<SessionResumeCapabilities>,
}
impl ::std::default::Default for SessionCapabilities {
    fn default() -> Self {
        Self {
            additional_directories: Default::default(),
            close: Default::default(),
            delete: Default::default(),
            list: Default::default(),
            meta: Default::default(),
            resume: Default::default(),
        }
    }
}
impl SessionCapabilities {
    pub fn builder() -> builder::SessionCapabilities {
        Default::default()
    }
}
#[doc = "Capabilities for the `session/close` method.\n\nSupplying `{}` means the agent supports closing sessions."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Capabilities for the `session/close` method.\\n\\nSupplying `{}` means the agent supports closing sessions.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct SessionCloseCapabilities {
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
}
impl ::std::default::Default for SessionCloseCapabilities {
    fn default() -> Self {
        Self {
            meta: Default::default(),
        }
    }
}
impl SessionCloseCapabilities {
    pub fn builder() -> builder::SessionCloseCapabilities {
        Default::default()
    }
}
#[doc = "A boolean on/off toggle session configuration option payload."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"A boolean on/off toggle session configuration option payload.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"currentValue\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"currentValue\": {"]
#[doc = "      \"description\": \"The current value of the boolean option.\","]
#[doc = "      \"type\": \"boolean\""]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct SessionConfigBoolean {
    #[doc = "The current value of the boolean option."]
    #[serde(rename = "currentValue")]
    pub current_value: bool,
}
impl SessionConfigBoolean {
    pub fn builder() -> builder::SessionConfigBoolean {
        Default::default()
    }
}
#[doc = "Unique identifier for a session configuration option value group."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Unique identifier for a session configuration option value group.\","]
#[doc = "  \"type\": \"string\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(
    :: serde :: Deserialize,
    :: serde :: Serialize,
    Clone,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
)]
#[serde(transparent)]
pub struct SessionConfigGroupId(pub ::std::string::String);
impl ::std::ops::Deref for SessionConfigGroupId {
    type Target = ::std::string::String;
    fn deref(&self) -> &::std::string::String {
        &self.0
    }
}
impl ::std::convert::From<SessionConfigGroupId> for ::std::string::String {
    fn from(value: SessionConfigGroupId) -> Self {
        value.0
    }
}
impl ::std::convert::From<::std::string::String> for SessionConfigGroupId {
    fn from(value: ::std::string::String) -> Self {
        Self(value)
    }
}
impl ::std::str::FromStr for SessionConfigGroupId {
    type Err = ::std::convert::Infallible;
    fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
        Ok(Self(value.to_string()))
    }
}
impl ::std::fmt::Display for SessionConfigGroupId {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        self.0.fmt(f)
    }
}
#[doc = "Unique identifier for a session configuration option."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Unique identifier for a session configuration option.\","]
#[doc = "  \"type\": \"string\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(
    :: serde :: Deserialize,
    :: serde :: Serialize,
    Clone,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
)]
#[serde(transparent)]
pub struct SessionConfigId(pub ::std::string::String);
impl ::std::ops::Deref for SessionConfigId {
    type Target = ::std::string::String;
    fn deref(&self) -> &::std::string::String {
        &self.0
    }
}
impl ::std::convert::From<SessionConfigId> for ::std::string::String {
    fn from(value: SessionConfigId) -> Self {
        value.0
    }
}
impl ::std::convert::From<::std::string::String> for SessionConfigId {
    fn from(value: ::std::string::String) -> Self {
        Self(value)
    }
}
impl ::std::str::FromStr for SessionConfigId {
    type Err = ::std::convert::Infallible;
    fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
        Ok(Self(value.to_string()))
    }
}
impl ::std::fmt::Display for SessionConfigId {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        self.0.fmt(f)
    }
}
#[doc = "A session configuration option selector and its current state."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"A session configuration option selector and its current state.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"oneOf\": ["]
#[doc = "    {"]
#[doc = "      \"description\": \"Single-value selector (dropdown).\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/SessionConfigSelect\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"required\": ["]
#[doc = "        \"type\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"type\": {"]
#[doc = "          \"type\": \"string\","]
#[doc = "          \"const\": \"select\""]
#[doc = "        }"]
#[doc = "      }"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"description\": \"Boolean on/off toggle.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/SessionConfigBoolean\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"required\": ["]
#[doc = "        \"type\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"type\": {"]
#[doc = "          \"type\": \"string\","]
#[doc = "          \"const\": \"boolean\""]
#[doc = "        }"]
#[doc = "      }"]
#[doc = "    }"]
#[doc = "  ],"]
#[doc = "  \"required\": ["]
#[doc = "    \"id\","]
#[doc = "    \"name\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"category\": {"]
#[doc = "      \"description\": \"Optional semantic category for this option (UX only).\","]
#[doc = "      \"anyOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/SessionConfigOptionCategory\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"type\": \"null\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"description\": {"]
#[doc = "      \"description\": \"Optional description for the Client to display to the user.\","]
#[doc = "      \"type\": ["]
#[doc = "        \"string\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"id\": {"]
#[doc = "      \"description\": \"Unique identifier for the configuration option.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/SessionConfigId\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    \"name\": {"]
#[doc = "      \"description\": \"Human-readable label for the option.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"discriminator\": {"]
#[doc = "    \"propertyName\": \"type\""]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
#[serde(untagged)]
pub enum SessionConfigOption {
    Variant0 {
        #[doc = "Optional semantic category for this option (UX only)."]
        #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
        category: ::std::option::Option<SessionConfigOptionCategory>,
        #[doc = "The currently selected value."]
        #[serde(rename = "currentValue")]
        current_value: SessionConfigValueId,
        #[doc = "Optional description for the Client to display to the user."]
        #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
        description: ::std::option::Option<::std::string::String>,
        #[doc = "Unique identifier for the configuration option."]
        id: SessionConfigId,
        #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
        #[serde(
            rename = "_meta",
            default,
            skip_serializing_if = "::std::option::Option::is_none"
        )]
        meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
        #[doc = "Human-readable label for the option."]
        name: ::std::string::String,
        #[doc = "The set of selectable options."]
        options: SessionConfigSelectOptions,
        #[serde(rename = "type")]
        type_: ::std::string::String,
    },
    Variant1 {
        #[doc = "Optional semantic category for this option (UX only)."]
        #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
        category: ::std::option::Option<SessionConfigOptionCategory>,
        #[doc = "The current value of the boolean option."]
        #[serde(rename = "currentValue")]
        current_value: bool,
        #[doc = "Optional description for the Client to display to the user."]
        #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
        description: ::std::option::Option<::std::string::String>,
        #[doc = "Unique identifier for the configuration option."]
        id: SessionConfigId,
        #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
        #[serde(
            rename = "_meta",
            default,
            skip_serializing_if = "::std::option::Option::is_none"
        )]
        meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
        #[doc = "Human-readable label for the option."]
        name: ::std::string::String,
        #[serde(rename = "type")]
        type_: ::std::string::String,
    },
}
#[doc = "Semantic category for a session configuration option.\n\nThis is intended to help Clients distinguish broadly common selectors (e.g. model selector vs\nsession mode selector vs thought/reasoning level) for UX purposes (keyboard shortcuts, icons,\nplacement). It MUST NOT be required for correctness. Clients MUST handle missing or unknown\ncategories gracefully.\n\nCategory names beginning with `_` are free for custom use, like other ACP extension methods.\nCategory names that do not begin with `_` are reserved for the ACP spec."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Semantic category for a session configuration option.\\n\\nThis is intended to help Clients distinguish broadly common selectors (e.g. model selector vs\\nsession mode selector vs thought/reasoning level) for UX purposes (keyboard shortcuts, icons,\\nplacement). It MUST NOT be required for correctness. Clients MUST handle missing or unknown\\ncategories gracefully.\\n\\nCategory names beginning with `_` are free for custom use, like other ACP extension methods.\\nCategory names that do not begin with `_` are reserved for the ACP spec.\","]
#[doc = "  \"anyOf\": ["]
#[doc = "    {"]
#[doc = "      \"description\": \"Session mode selector.\","]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"const\": \"mode\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"description\": \"Model selector.\","]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"const\": \"model\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"description\": \"Model-related configuration parameter.\","]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"const\": \"model_config\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"description\": \"Thought/reasoning level selector.\","]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"const\": \"thought_level\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"other\","]
#[doc = "      \"description\": \"Unknown / uncategorized selector.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    }"]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct SessionConfigOptionCategory {
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_0: ::std::option::Option<::std::string::String>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_1: ::std::option::Option<::std::string::String>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_2: ::std::option::Option<::std::string::String>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_3: ::std::option::Option<::std::string::String>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_4: ::std::option::Option<::std::string::String>,
}
impl ::std::default::Default for SessionConfigOptionCategory {
    fn default() -> Self {
        Self {
            subtype_0: Default::default(),
            subtype_1: Default::default(),
            subtype_2: Default::default(),
            subtype_3: Default::default(),
            subtype_4: Default::default(),
        }
    }
}
impl SessionConfigOptionCategory {
    pub fn builder() -> builder::SessionConfigOptionCategory {
        Default::default()
    }
}
#[doc = "Session configuration option capabilities supported by the client."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Session configuration option capabilities supported by the client.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"boolean\": {"]
#[doc = "      \"description\": \"Whether the client supports boolean session configuration options.\\n\\nOptional. Omitted or `null` both mean the client does not advertise support.\\nSupplying `{}` means agents may include `type: \\\"boolean\\\"` entries in\\n`configOptions`, and the client may send `session/set_config_option`\\nrequests with `type: \\\"boolean\\\"` and a boolean `value`.\","]
#[doc = "      \"anyOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/BooleanConfigOptionCapabilities\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"type\": \"null\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct SessionConfigOptionsCapabilities {
    #[doc = "Whether the client supports boolean session configuration options.\n\nOptional. Omitted or `null` both mean the client does not advertise support.\nSupplying `{}` means agents may include `type: \"boolean\"` entries in\n`configOptions`, and the client may send `session/set_config_option`\nrequests with `type: \"boolean\"` and a boolean `value`."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub boolean: ::std::option::Option<BooleanConfigOptionCapabilities>,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
}
impl ::std::default::Default for SessionConfigOptionsCapabilities {
    fn default() -> Self {
        Self {
            boolean: Default::default(),
            meta: Default::default(),
        }
    }
}
impl SessionConfigOptionsCapabilities {
    pub fn builder() -> builder::SessionConfigOptionsCapabilities {
        Default::default()
    }
}
#[doc = "A single-value selector (dropdown) session configuration option payload."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"A single-value selector (dropdown) session configuration option payload.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"currentValue\","]
#[doc = "    \"options\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"currentValue\": {"]
#[doc = "      \"description\": \"The currently selected value.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/SessionConfigValueId\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    \"options\": {"]
#[doc = "      \"description\": \"The set of selectable options.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/SessionConfigSelectOptions\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct SessionConfigSelect {
    #[doc = "The currently selected value."]
    #[serde(rename = "currentValue")]
    pub current_value: SessionConfigValueId,
    #[doc = "The set of selectable options."]
    pub options: SessionConfigSelectOptions,
}
impl SessionConfigSelect {
    pub fn builder() -> builder::SessionConfigSelect {
        Default::default()
    }
}
#[doc = "A group of possible values for a session configuration option."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"A group of possible values for a session configuration option.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"group\","]
#[doc = "    \"name\","]
#[doc = "    \"options\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"group\": {"]
#[doc = "      \"description\": \"Unique identifier for this group.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/SessionConfigGroupId\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    \"name\": {"]
#[doc = "      \"description\": \"Human-readable label for this group.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"options\": {"]
#[doc = "      \"description\": \"The set of option values in this group.\","]
#[doc = "      \"type\": \"array\","]
#[doc = "      \"items\": {"]
#[doc = "        \"$ref\": \"#/$defs/SessionConfigSelectOption\""]
#[doc = "      },"]
#[doc = "      \"x-deserialize-default-on-error\": true,"]
#[doc = "      \"x-deserialize-skip-invalid-items\": true"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct SessionConfigSelectGroup {
    #[doc = "Unique identifier for this group."]
    pub group: SessionConfigGroupId,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "Human-readable label for this group."]
    pub name: ::std::string::String,
    #[doc = "The set of option values in this group."]
    pub options: ::std::vec::Vec<SessionConfigSelectOption>,
}
impl SessionConfigSelectGroup {
    pub fn builder() -> builder::SessionConfigSelectGroup {
        Default::default()
    }
}
#[doc = "A possible value for a session configuration option."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"A possible value for a session configuration option.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"name\","]
#[doc = "    \"value\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"description\": {"]
#[doc = "      \"description\": \"Optional description for this option value.\","]
#[doc = "      \"type\": ["]
#[doc = "        \"string\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"name\": {"]
#[doc = "      \"description\": \"Human-readable label for this option value.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"value\": {"]
#[doc = "      \"description\": \"Unique identifier for this option value.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/SessionConfigValueId\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct SessionConfigSelectOption {
    #[doc = "Optional description for this option value."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub description: ::std::option::Option<::std::string::String>,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "Human-readable label for this option value."]
    pub name: ::std::string::String,
    #[doc = "Unique identifier for this option value."]
    pub value: SessionConfigValueId,
}
impl SessionConfigSelectOption {
    pub fn builder() -> builder::SessionConfigSelectOption {
        Default::default()
    }
}
#[doc = "Possible values for a session configuration option."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Possible values for a session configuration option.\","]
#[doc = "  \"anyOf\": ["]
#[doc = "    {"]
#[doc = "      \"title\": \"Ungrouped\","]
#[doc = "      \"description\": \"A flat list of options with no grouping.\","]
#[doc = "      \"type\": \"array\","]
#[doc = "      \"items\": {"]
#[doc = "        \"$ref\": \"#/$defs/SessionConfigSelectOption\""]
#[doc = "      }"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"Grouped\","]
#[doc = "      \"description\": \"A list of options grouped under headers.\","]
#[doc = "      \"type\": \"array\","]
#[doc = "      \"items\": {"]
#[doc = "        \"$ref\": \"#/$defs/SessionConfigSelectGroup\""]
#[doc = "      }"]
#[doc = "    }"]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct SessionConfigSelectOptions {
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_0: ::std::option::Option<::std::vec::Vec<SessionConfigSelectOption>>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_1: ::std::option::Option<::std::vec::Vec<SessionConfigSelectGroup>>,
}
impl ::std::default::Default for SessionConfigSelectOptions {
    fn default() -> Self {
        Self {
            subtype_0: Default::default(),
            subtype_1: Default::default(),
        }
    }
}
impl SessionConfigSelectOptions {
    pub fn builder() -> builder::SessionConfigSelectOptions {
        Default::default()
    }
}
#[doc = "Unique identifier for a session configuration option value."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Unique identifier for a session configuration option value.\","]
#[doc = "  \"type\": \"string\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(
    :: serde :: Deserialize,
    :: serde :: Serialize,
    Clone,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
)]
#[serde(transparent)]
pub struct SessionConfigValueId(pub ::std::string::String);
impl ::std::ops::Deref for SessionConfigValueId {
    type Target = ::std::string::String;
    fn deref(&self) -> &::std::string::String {
        &self.0
    }
}
impl ::std::convert::From<SessionConfigValueId> for ::std::string::String {
    fn from(value: SessionConfigValueId) -> Self {
        value.0
    }
}
impl ::std::convert::From<::std::string::String> for SessionConfigValueId {
    fn from(value: ::std::string::String) -> Self {
        Self(value)
    }
}
impl ::std::str::FromStr for SessionConfigValueId {
    type Err = ::std::convert::Infallible;
    fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
        Ok(Self(value.to_string()))
    }
}
impl ::std::fmt::Display for SessionConfigValueId {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        self.0.fmt(f)
    }
}
#[doc = "Capabilities for the `session/delete` method.\n\nSupplying `{}` means the agent supports deleting sessions from `session/list`."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Capabilities for the `session/delete` method.\\n\\nSupplying `{}` means the agent supports deleting sessions from `session/list`.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct SessionDeleteCapabilities {
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
}
impl ::std::default::Default for SessionDeleteCapabilities {
    fn default() -> Self {
        Self {
            meta: Default::default(),
        }
    }
}
impl SessionDeleteCapabilities {
    pub fn builder() -> builder::SessionDeleteCapabilities {
        Default::default()
    }
}
#[doc = "A unique identifier for a conversation session between a client and agent.\n\nSessions maintain their own context, conversation history, and state,\nallowing multiple independent interactions with the same agent.\n\nSee protocol docs: [Session ID](https://agentclientprotocol.com/protocol/session-setup#session-id)"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"A unique identifier for a conversation session between a client and agent.\\n\\nSessions maintain their own context, conversation history, and state,\\nallowing multiple independent interactions with the same agent.\\n\\nSee protocol docs: [Session ID](https://agentclientprotocol.com/protocol/session-setup#session-id)\","]
#[doc = "  \"type\": \"string\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(
    :: serde :: Deserialize,
    :: serde :: Serialize,
    Clone,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
)]
#[serde(transparent)]
pub struct SessionId(pub ::std::string::String);
impl ::std::ops::Deref for SessionId {
    type Target = ::std::string::String;
    fn deref(&self) -> &::std::string::String {
        &self.0
    }
}
impl ::std::convert::From<SessionId> for ::std::string::String {
    fn from(value: SessionId) -> Self {
        value.0
    }
}
impl ::std::convert::From<::std::string::String> for SessionId {
    fn from(value: ::std::string::String) -> Self {
        Self(value)
    }
}
impl ::std::str::FromStr for SessionId {
    type Err = ::std::convert::Infallible;
    fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
        Ok(Self(value.to_string()))
    }
}
impl ::std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        self.0.fmt(f)
    }
}
#[doc = "Information about a session returned by session/list"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Information about a session returned by session/list\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"cwd\","]
#[doc = "    \"sessionId\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"additionalDirectories\": {"]
#[doc = "      \"description\": \"Additional workspace roots reported for this session. Each path must be absolute.\\n\\nWhen present, this is the complete ordered additional-root list reported\\nby the Agent. Omitted and empty values are equivalent: the response\\nreports no additional roots.\","]
#[doc = "      \"type\": \"array\","]
#[doc = "      \"items\": {"]
#[doc = "        \"type\": \"string\""]
#[doc = "      },"]
#[doc = "      \"x-deserialize-default-on-error\": true,"]
#[doc = "      \"x-deserialize-skip-invalid-items\": true"]
#[doc = "    },"]
#[doc = "    \"cwd\": {"]
#[doc = "      \"description\": \"The working directory for this session. Must be an absolute path.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"sessionId\": {"]
#[doc = "      \"description\": \"Unique identifier for the session\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/SessionId\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    \"title\": {"]
#[doc = "      \"description\": \"Human-readable title for the session\","]
#[doc = "      \"type\": ["]
#[doc = "        \"string\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"updatedAt\": {"]
#[doc = "      \"description\": \"ISO 8601 timestamp of last activity\","]
#[doc = "      \"type\": ["]
#[doc = "        \"string\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct SessionInfo {
    #[doc = "Additional workspace roots reported for this session. Each path must be absolute.\n\nWhen present, this is the complete ordered additional-root list reported\nby the Agent. Omitted and empty values are equivalent: the response\nreports no additional roots."]
    #[serde(
        rename = "additionalDirectories",
        default,
        skip_serializing_if = "::std::vec::Vec::is_empty"
    )]
    pub additional_directories: ::std::vec::Vec<::std::string::String>,
    #[doc = "The working directory for this session. Must be an absolute path."]
    pub cwd: ::std::string::String,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "Unique identifier for the session"]
    #[serde(rename = "sessionId")]
    pub session_id: SessionId,
    #[doc = "Human-readable title for the session"]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub title: ::std::option::Option<::std::string::String>,
    #[doc = "ISO 8601 timestamp of last activity"]
    #[serde(
        rename = "updatedAt",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub updated_at: ::std::option::Option<::std::string::String>,
}
impl SessionInfo {
    pub fn builder() -> builder::SessionInfo {
        Default::default()
    }
}
#[doc = "Update to session metadata. All fields are optional to support partial updates.\n\nAgents send this notification to update session information like title or custom metadata.\nThis allows clients to display dynamic session names and track session state changes."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Update to session metadata. All fields are optional to support partial updates.\\n\\nAgents send this notification to update session information like title or custom metadata.\\nThis allows clients to display dynamic session names and track session state changes.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"title\": {"]
#[doc = "      \"description\": \"Human-readable title for the session. Set to null to clear.\","]
#[doc = "      \"type\": ["]
#[doc = "        \"string\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"updatedAt\": {"]
#[doc = "      \"description\": \"ISO 8601 timestamp of last activity. Set to null to clear.\","]
#[doc = "      \"type\": ["]
#[doc = "        \"string\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct SessionInfoUpdate {
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "Human-readable title for the session. Set to null to clear."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub title: ::std::option::Option<::std::string::String>,
    #[doc = "ISO 8601 timestamp of last activity. Set to null to clear."]
    #[serde(
        rename = "updatedAt",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub updated_at: ::std::option::Option<::std::string::String>,
}
impl ::std::default::Default for SessionInfoUpdate {
    fn default() -> Self {
        Self {
            meta: Default::default(),
            title: Default::default(),
            updated_at: Default::default(),
        }
    }
}
impl SessionInfoUpdate {
    pub fn builder() -> builder::SessionInfoUpdate {
        Default::default()
    }
}
#[doc = "Capabilities for the `session/list` method.\n\nSupplying `{}` means the agent supports listing sessions."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Capabilities for the `session/list` method.\\n\\nSupplying `{}` means the agent supports listing sessions.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct SessionListCapabilities {
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
}
impl ::std::default::Default for SessionListCapabilities {
    fn default() -> Self {
        Self {
            meta: Default::default(),
        }
    }
}
impl SessionListCapabilities {
    pub fn builder() -> builder::SessionListCapabilities {
        Default::default()
    }
}
#[doc = "A mode the agent can operate in.\n\nSee protocol docs: [Session Modes](https://agentclientprotocol.com/protocol/session-modes)"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"A mode the agent can operate in.\\n\\nSee protocol docs: [Session Modes](https://agentclientprotocol.com/protocol/session-modes)\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"id\","]
#[doc = "    \"name\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"description\": {"]
#[doc = "      \"description\": \"Optional human-readable details shown with this protocol object.\","]
#[doc = "      \"type\": ["]
#[doc = "        \"string\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"id\": {"]
#[doc = "      \"description\": \"Stable identifier used to refer to this protocol object in later messages.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/SessionModeId\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    \"name\": {"]
#[doc = "      \"description\": \"Human-readable name shown for this protocol object.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct SessionMode {
    #[doc = "Optional human-readable details shown with this protocol object."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub description: ::std::option::Option<::std::string::String>,
    #[doc = "Stable identifier used to refer to this protocol object in later messages."]
    pub id: SessionModeId,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "Human-readable name shown for this protocol object."]
    pub name: ::std::string::String,
}
impl SessionMode {
    pub fn builder() -> builder::SessionMode {
        Default::default()
    }
}
#[doc = "Unique identifier for a Session Mode."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Unique identifier for a Session Mode.\","]
#[doc = "  \"type\": \"string\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(
    :: serde :: Deserialize,
    :: serde :: Serialize,
    Clone,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
)]
#[serde(transparent)]
pub struct SessionModeId(pub ::std::string::String);
impl ::std::ops::Deref for SessionModeId {
    type Target = ::std::string::String;
    fn deref(&self) -> &::std::string::String {
        &self.0
    }
}
impl ::std::convert::From<SessionModeId> for ::std::string::String {
    fn from(value: SessionModeId) -> Self {
        value.0
    }
}
impl ::std::convert::From<::std::string::String> for SessionModeId {
    fn from(value: ::std::string::String) -> Self {
        Self(value)
    }
}
impl ::std::str::FromStr for SessionModeId {
    type Err = ::std::convert::Infallible;
    fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
        Ok(Self(value.to_string()))
    }
}
impl ::std::fmt::Display for SessionModeId {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        self.0.fmt(f)
    }
}
#[doc = "The set of modes and the one currently active."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"The set of modes and the one currently active.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"availableModes\","]
#[doc = "    \"currentModeId\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"availableModes\": {"]
#[doc = "      \"description\": \"The set of modes that the Agent can operate in\","]
#[doc = "      \"type\": \"array\","]
#[doc = "      \"items\": {"]
#[doc = "        \"$ref\": \"#/$defs/SessionMode\""]
#[doc = "      },"]
#[doc = "      \"x-deserialize-default-on-error\": true,"]
#[doc = "      \"x-deserialize-skip-invalid-items\": true"]
#[doc = "    },"]
#[doc = "    \"currentModeId\": {"]
#[doc = "      \"description\": \"The current mode the Agent is in.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/SessionModeId\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct SessionModeState {
    #[doc = "The set of modes that the Agent can operate in"]
    #[serde(rename = "availableModes")]
    pub available_modes: ::std::vec::Vec<SessionMode>,
    #[doc = "The current mode the Agent is in."]
    #[serde(rename = "currentModeId")]
    pub current_mode_id: SessionModeId,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
}
impl SessionModeState {
    pub fn builder() -> builder::SessionModeState {
        Default::default()
    }
}
#[doc = "Notification containing a session update from the agent.\n\nUsed to stream real-time progress and results during prompt processing.\n\nSee protocol docs: [Agent Reports Output](https://agentclientprotocol.com/protocol/prompt-turn#3-agent-reports-output)"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Notification containing a session update from the agent.\\n\\nUsed to stream real-time progress and results during prompt processing.\\n\\nSee protocol docs: [Agent Reports Output](https://agentclientprotocol.com/protocol/prompt-turn#3-agent-reports-output)\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"sessionId\","]
#[doc = "    \"update\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"sessionId\": {"]
#[doc = "      \"description\": \"The ID of the session this update pertains to.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/SessionId\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    \"update\": {"]
#[doc = "      \"description\": \"The actual update content.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/SessionUpdate\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"x-method\": \"session/update\","]
#[doc = "  \"x-side\": \"client\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct SessionNotification {
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "The ID of the session this update pertains to."]
    #[serde(rename = "sessionId")]
    pub session_id: SessionId,
    #[doc = "The actual update content."]
    pub update: SessionUpdate,
}
impl SessionNotification {
    pub fn builder() -> builder::SessionNotification {
        Default::default()
    }
}
#[doc = "Capabilities for the `session/resume` method.\n\nSupplying `{}` means the agent supports resuming sessions."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Capabilities for the `session/resume` method.\\n\\nSupplying `{}` means the agent supports resuming sessions.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct SessionResumeCapabilities {
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
}
impl ::std::default::Default for SessionResumeCapabilities {
    fn default() -> Self {
        Self {
            meta: Default::default(),
        }
    }
}
impl SessionResumeCapabilities {
    pub fn builder() -> builder::SessionResumeCapabilities {
        Default::default()
    }
}
#[doc = "Different types of updates that can be sent during session processing.\n\nThese updates provide real-time feedback about the agent's progress.\n\nSee protocol docs: [Agent Reports Output](https://agentclientprotocol.com/protocol/prompt-turn#3-agent-reports-output)"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Different types of updates that can be sent during session processing.\\n\\nThese updates provide real-time feedback about the agent's progress.\\n\\nSee protocol docs: [Agent Reports Output](https://agentclientprotocol.com/protocol/prompt-turn#3-agent-reports-output)\","]
#[doc = "  \"oneOf\": ["]
#[doc = "    {"]
#[doc = "      \"description\": \"A chunk of the user's message being streamed.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/ContentChunk\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"required\": ["]
#[doc = "        \"sessionUpdate\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"sessionUpdate\": {"]
#[doc = "          \"type\": \"string\","]
#[doc = "          \"const\": \"user_message_chunk\""]
#[doc = "        }"]
#[doc = "      }"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"description\": \"A chunk of the agent's response being streamed.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/ContentChunk\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"required\": ["]
#[doc = "        \"sessionUpdate\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"sessionUpdate\": {"]
#[doc = "          \"type\": \"string\","]
#[doc = "          \"const\": \"agent_message_chunk\""]
#[doc = "        }"]
#[doc = "      }"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"description\": \"A chunk of the agent's internal reasoning being streamed.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/ContentChunk\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"required\": ["]
#[doc = "        \"sessionUpdate\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"sessionUpdate\": {"]
#[doc = "          \"type\": \"string\","]
#[doc = "          \"const\": \"agent_thought_chunk\""]
#[doc = "        }"]
#[doc = "      }"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"description\": \"Notification that a new tool call has been initiated.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/ToolCall\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"required\": ["]
#[doc = "        \"sessionUpdate\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"sessionUpdate\": {"]
#[doc = "          \"type\": \"string\","]
#[doc = "          \"const\": \"tool_call\""]
#[doc = "        }"]
#[doc = "      }"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"description\": \"Update on the status or results of a tool call.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/ToolCallUpdate\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"required\": ["]
#[doc = "        \"sessionUpdate\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"sessionUpdate\": {"]
#[doc = "          \"type\": \"string\","]
#[doc = "          \"const\": \"tool_call_update\""]
#[doc = "        }"]
#[doc = "      }"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"description\": \"The agent's execution plan for complex tasks.\\nSee protocol docs: [Agent Plan](https://agentclientprotocol.com/protocol/agent-plan)\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/Plan\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"required\": ["]
#[doc = "        \"sessionUpdate\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"sessionUpdate\": {"]
#[doc = "          \"type\": \"string\","]
#[doc = "          \"const\": \"plan\""]
#[doc = "        }"]
#[doc = "      }"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"description\": \"Available commands are ready or have changed\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/AvailableCommandsUpdate\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"required\": ["]
#[doc = "        \"sessionUpdate\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"sessionUpdate\": {"]
#[doc = "          \"type\": \"string\","]
#[doc = "          \"const\": \"available_commands_update\""]
#[doc = "        }"]
#[doc = "      }"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"description\": \"The current mode of the session has changed\\n\\nSee protocol docs: [Session Modes](https://agentclientprotocol.com/protocol/session-modes)\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/CurrentModeUpdate\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"required\": ["]
#[doc = "        \"sessionUpdate\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"sessionUpdate\": {"]
#[doc = "          \"type\": \"string\","]
#[doc = "          \"const\": \"current_mode_update\""]
#[doc = "        }"]
#[doc = "      }"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"description\": \"Session configuration options have been updated.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/ConfigOptionUpdate\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"required\": ["]
#[doc = "        \"sessionUpdate\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"sessionUpdate\": {"]
#[doc = "          \"type\": \"string\","]
#[doc = "          \"const\": \"config_option_update\""]
#[doc = "        }"]
#[doc = "      }"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"description\": \"Session metadata has been updated (title, timestamps, custom metadata)\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/SessionInfoUpdate\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"required\": ["]
#[doc = "        \"sessionUpdate\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"sessionUpdate\": {"]
#[doc = "          \"type\": \"string\","]
#[doc = "          \"const\": \"session_info_update\""]
#[doc = "        }"]
#[doc = "      }"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"description\": \"Context window and cost update for the session.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/UsageUpdate\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"required\": ["]
#[doc = "        \"sessionUpdate\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"sessionUpdate\": {"]
#[doc = "          \"type\": \"string\","]
#[doc = "          \"const\": \"usage_update\""]
#[doc = "        }"]
#[doc = "      }"]
#[doc = "    }"]
#[doc = "  ],"]
#[doc = "  \"discriminator\": {"]
#[doc = "    \"propertyName\": \"sessionUpdate\""]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
#[serde(untagged)]
pub enum SessionUpdate {
    Variant0 {
        #[doc = "A single item of content"]
        content: ContentBlock,
        #[doc = "A unique identifier for the message this chunk belongs to.\n\nAll chunks belonging to the same message share the same `messageId`.\nA change in `messageId` indicates a new message has started."]
        #[serde(
            rename = "messageId",
            default,
            skip_serializing_if = "::std::option::Option::is_none"
        )]
        message_id: ::std::option::Option<MessageId>,
        #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
        #[serde(
            rename = "_meta",
            default,
            skip_serializing_if = "::std::option::Option::is_none"
        )]
        meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
        #[serde(rename = "sessionUpdate")]
        session_update: ::std::string::String,
    },
    Variant1 {
        #[doc = "A single item of content"]
        content: ContentBlock,
        #[doc = "A unique identifier for the message this chunk belongs to.\n\nAll chunks belonging to the same message share the same `messageId`.\nA change in `messageId` indicates a new message has started."]
        #[serde(
            rename = "messageId",
            default,
            skip_serializing_if = "::std::option::Option::is_none"
        )]
        message_id: ::std::option::Option<MessageId>,
        #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
        #[serde(
            rename = "_meta",
            default,
            skip_serializing_if = "::std::option::Option::is_none"
        )]
        meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
        #[serde(rename = "sessionUpdate")]
        session_update: ::std::string::String,
    },
    Variant2 {
        #[doc = "A single item of content"]
        content: ContentBlock,
        #[doc = "A unique identifier for the message this chunk belongs to.\n\nAll chunks belonging to the same message share the same `messageId`.\nA change in `messageId` indicates a new message has started."]
        #[serde(
            rename = "messageId",
            default,
            skip_serializing_if = "::std::option::Option::is_none"
        )]
        message_id: ::std::option::Option<MessageId>,
        #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
        #[serde(
            rename = "_meta",
            default,
            skip_serializing_if = "::std::option::Option::is_none"
        )]
        meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
        #[serde(rename = "sessionUpdate")]
        session_update: ::std::string::String,
    },
    Variant3 {
        #[doc = "Content produced by the tool call."]
        #[serde(default, skip_serializing_if = "::std::vec::Vec::is_empty")]
        content: ::std::vec::Vec<ToolCallContent>,
        #[doc = "The category of tool being invoked.\nHelps clients choose appropriate icons and UI treatment."]
        #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
        kind: ::std::option::Option<ToolKind>,
        #[doc = "File locations affected by this tool call.\nEnables \"follow-along\" features in clients."]
        #[serde(default, skip_serializing_if = "::std::vec::Vec::is_empty")]
        locations: ::std::vec::Vec<ToolCallLocation>,
        #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
        #[serde(
            rename = "_meta",
            default,
            skip_serializing_if = "::std::option::Option::is_none"
        )]
        meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
        #[doc = "Raw input parameters sent to the tool."]
        #[serde(
            rename = "rawInput",
            default,
            skip_serializing_if = "::std::option::Option::is_none"
        )]
        raw_input: ::std::option::Option<::serde_json::Value>,
        #[doc = "Raw output returned by the tool."]
        #[serde(
            rename = "rawOutput",
            default,
            skip_serializing_if = "::std::option::Option::is_none"
        )]
        raw_output: ::std::option::Option<::serde_json::Value>,
        #[serde(rename = "sessionUpdate")]
        session_update: ::std::string::String,
        #[doc = "Current execution status of the tool call."]
        #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
        status: ::std::option::Option<ToolCallStatus>,
        #[doc = "Human-readable title describing what the tool is doing."]
        title: ::std::string::String,
        #[doc = "Unique identifier for this tool call within the session."]
        #[serde(rename = "toolCallId")]
        tool_call_id: ToolCallId,
    },
    Variant4 {
        #[doc = "Replace the content collection."]
        #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
        content: ::std::option::Option<::std::vec::Vec<ToolCallContent>>,
        #[doc = "Update the tool kind."]
        #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
        kind: ::std::option::Option<ToolKind>,
        #[doc = "Replace the locations collection."]
        #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
        locations: ::std::option::Option<::std::vec::Vec<ToolCallLocation>>,
        #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
        #[serde(
            rename = "_meta",
            default,
            skip_serializing_if = "::std::option::Option::is_none"
        )]
        meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
        #[doc = "Update the raw input."]
        #[serde(
            rename = "rawInput",
            default,
            skip_serializing_if = "::std::option::Option::is_none"
        )]
        raw_input: ::std::option::Option<::serde_json::Value>,
        #[doc = "Update the raw output."]
        #[serde(
            rename = "rawOutput",
            default,
            skip_serializing_if = "::std::option::Option::is_none"
        )]
        raw_output: ::std::option::Option<::serde_json::Value>,
        #[serde(rename = "sessionUpdate")]
        session_update: ::std::string::String,
        #[doc = "Update the execution status."]
        #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
        status: ::std::option::Option<ToolCallStatus>,
        #[doc = "Update the human-readable title."]
        #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
        title: ::std::option::Option<::std::string::String>,
        #[doc = "The ID of the tool call being updated."]
        #[serde(rename = "toolCallId")]
        tool_call_id: ToolCallId,
    },
    Variant5 {
        #[doc = "The list of tasks to be accomplished.\n\nWhen updating a plan, the agent must send a complete list of all entries\nwith their current status. The client replaces the entire plan with each update."]
        entries: ::std::vec::Vec<PlanEntry>,
        #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
        #[serde(
            rename = "_meta",
            default,
            skip_serializing_if = "::std::option::Option::is_none"
        )]
        meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
        #[serde(rename = "sessionUpdate")]
        session_update: ::std::string::String,
    },
    Variant6 {
        #[doc = "Commands the agent can execute"]
        #[serde(rename = "availableCommands")]
        available_commands: ::std::vec::Vec<AvailableCommand>,
        #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
        #[serde(
            rename = "_meta",
            default,
            skip_serializing_if = "::std::option::Option::is_none"
        )]
        meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
        #[serde(rename = "sessionUpdate")]
        session_update: ::std::string::String,
    },
    Variant7 {
        #[doc = "The ID of the current mode"]
        #[serde(rename = "currentModeId")]
        current_mode_id: SessionModeId,
        #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
        #[serde(
            rename = "_meta",
            default,
            skip_serializing_if = "::std::option::Option::is_none"
        )]
        meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
        #[serde(rename = "sessionUpdate")]
        session_update: ::std::string::String,
    },
    Variant8 {
        #[doc = "The full set of configuration options and their current values."]
        #[serde(rename = "configOptions")]
        config_options: ::std::vec::Vec<SessionConfigOption>,
        #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
        #[serde(
            rename = "_meta",
            default,
            skip_serializing_if = "::std::option::Option::is_none"
        )]
        meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
        #[serde(rename = "sessionUpdate")]
        session_update: ::std::string::String,
    },
    Variant9 {
        #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
        #[serde(
            rename = "_meta",
            default,
            skip_serializing_if = "::std::option::Option::is_none"
        )]
        meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
        #[serde(rename = "sessionUpdate")]
        session_update: ::std::string::String,
        #[doc = "Human-readable title for the session. Set to null to clear."]
        #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
        title: ::std::option::Option<::std::string::String>,
        #[doc = "ISO 8601 timestamp of last activity. Set to null to clear."]
        #[serde(
            rename = "updatedAt",
            default,
            skip_serializing_if = "::std::option::Option::is_none"
        )]
        updated_at: ::std::option::Option<::std::string::String>,
    },
    Variant10 {
        #[doc = "Cumulative session cost (optional)."]
        #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
        cost: ::std::option::Option<Cost>,
        #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
        #[serde(
            rename = "_meta",
            default,
            skip_serializing_if = "::std::option::Option::is_none"
        )]
        meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
        #[serde(rename = "sessionUpdate")]
        session_update: ::std::string::String,
        #[doc = "Total context window size in tokens."]
        size: u64,
        #[doc = "Tokens currently in context."]
        used: u64,
    },
}
#[doc = "Request parameters for setting a session configuration option."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Request parameters for setting a session configuration option.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"anyOf\": ["]
#[doc = "    {"]
#[doc = "      \"description\": \"A boolean value (`type: \\\"boolean\\\"`).\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"required\": ["]
#[doc = "        \"type\","]
#[doc = "        \"value\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"type\": {"]
#[doc = "          \"type\": \"string\","]
#[doc = "          \"const\": \"boolean\""]
#[doc = "        },"]
#[doc = "        \"value\": {"]
#[doc = "          \"description\": \"The boolean value.\","]
#[doc = "          \"type\": \"boolean\""]
#[doc = "        }"]
#[doc = "      }"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"value_id\","]
#[doc = "      \"description\": \"A [`SessionConfigValueId`] string value.\\n\\nThis is the default when `type` is absent on the wire. Unknown `type`\\nvalues with string payloads also gracefully deserialize into this\\nvariant.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"required\": ["]
#[doc = "        \"value\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"value\": {"]
#[doc = "          \"description\": \"The value ID.\","]
#[doc = "          \"allOf\": ["]
#[doc = "            {"]
#[doc = "              \"$ref\": \"#/$defs/SessionConfigValueId\""]
#[doc = "            }"]
#[doc = "          ]"]
#[doc = "        }"]
#[doc = "      }"]
#[doc = "    }"]
#[doc = "  ],"]
#[doc = "  \"required\": ["]
#[doc = "    \"configId\","]
#[doc = "    \"sessionId\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"configId\": {"]
#[doc = "      \"description\": \"The ID of the configuration option to set.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/SessionConfigId\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    \"sessionId\": {"]
#[doc = "      \"description\": \"The ID of the session to set the configuration option for.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/SessionId\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"x-method\": \"session/set_config_option\","]
#[doc = "  \"x-side\": \"agent\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
#[serde(untagged)]
pub enum SetSessionConfigOptionRequest {
    Variant0 {
        #[doc = "The ID of the configuration option to set."]
        #[serde(rename = "configId")]
        config_id: SessionConfigId,
        #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
        #[serde(
            rename = "_meta",
            default,
            skip_serializing_if = "::std::option::Option::is_none"
        )]
        meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
        #[doc = "The ID of the session to set the configuration option for."]
        #[serde(rename = "sessionId")]
        session_id: SessionId,
        #[serde(rename = "type")]
        type_: ::std::string::String,
        #[doc = "The boolean value."]
        value: bool,
    },
    Variant1 {
        #[doc = "The ID of the configuration option to set."]
        #[serde(rename = "configId")]
        config_id: SessionConfigId,
        #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
        #[serde(
            rename = "_meta",
            default,
            skip_serializing_if = "::std::option::Option::is_none"
        )]
        meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
        #[doc = "The ID of the session to set the configuration option for."]
        #[serde(rename = "sessionId")]
        session_id: SessionId,
        #[doc = "The value ID."]
        value: SessionConfigValueId,
    },
}
#[doc = "Response to `session/set_config_option` method."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Response to `session/set_config_option` method.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"configOptions\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"configOptions\": {"]
#[doc = "      \"description\": \"The full set of configuration options and their current values.\","]
#[doc = "      \"type\": \"array\","]
#[doc = "      \"items\": {"]
#[doc = "        \"$ref\": \"#/$defs/SessionConfigOption\""]
#[doc = "      },"]
#[doc = "      \"x-deserialize-default-on-error\": true,"]
#[doc = "      \"x-deserialize-skip-invalid-items\": true"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"x-method\": \"session/set_config_option\","]
#[doc = "  \"x-side\": \"agent\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct SetSessionConfigOptionResponse {
    #[doc = "The full set of configuration options and their current values."]
    #[serde(rename = "configOptions")]
    pub config_options: ::std::vec::Vec<SessionConfigOption>,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
}
impl SetSessionConfigOptionResponse {
    pub fn builder() -> builder::SetSessionConfigOptionResponse {
        Default::default()
    }
}
#[doc = "Request parameters for setting a session mode."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Request parameters for setting a session mode.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"modeId\","]
#[doc = "    \"sessionId\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"modeId\": {"]
#[doc = "      \"description\": \"The ID of the mode to set.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/SessionModeId\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    \"sessionId\": {"]
#[doc = "      \"description\": \"The ID of the session to set the mode for.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/SessionId\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"x-method\": \"session/set_mode\","]
#[doc = "  \"x-side\": \"agent\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct SetSessionModeRequest {
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "The ID of the mode to set."]
    #[serde(rename = "modeId")]
    pub mode_id: SessionModeId,
    #[doc = "The ID of the session to set the mode for."]
    #[serde(rename = "sessionId")]
    pub session_id: SessionId,
}
impl SetSessionModeRequest {
    pub fn builder() -> builder::SetSessionModeRequest {
        Default::default()
    }
}
#[doc = "Response to `session/set_mode` method."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Response to `session/set_mode` method.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"x-method\": \"session/set_mode\","]
#[doc = "  \"x-side\": \"agent\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct SetSessionModeResponse {
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
}
impl ::std::default::Default for SetSessionModeResponse {
    fn default() -> Self {
        Self {
            meta: Default::default(),
        }
    }
}
impl SetSessionModeResponse {
    pub fn builder() -> builder::SetSessionModeResponse {
        Default::default()
    }
}
#[doc = "Reasons why an agent stops processing a prompt turn.\n\nSee protocol docs: [Stop Reasons](https://agentclientprotocol.com/protocol/prompt-turn#stop-reasons)"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Reasons why an agent stops processing a prompt turn.\\n\\nSee protocol docs: [Stop Reasons](https://agentclientprotocol.com/protocol/prompt-turn#stop-reasons)\","]
#[doc = "  \"oneOf\": ["]
#[doc = "    {"]
#[doc = "      \"description\": \"The turn ended successfully.\","]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"const\": \"end_turn\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"description\": \"The turn ended because the agent reached the maximum number of tokens.\","]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"const\": \"max_tokens\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"description\": \"The turn ended because the agent reached the maximum number of allowed\\nagent requests between user turns.\","]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"const\": \"max_turn_requests\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"description\": \"The turn ended because the agent refused to continue. The user prompt\\nand everything that comes after it won't be included in the next\\nprompt, so this should be reflected in the UI.\","]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"const\": \"refusal\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"description\": \"The turn was cancelled by the client via `session/cancel`.\\n\\nThis stop reason MUST be returned when the client sends a `session/cancel`\\nnotification, even if the cancellation causes exceptions in underlying operations.\\nAgents should catch these exceptions and return this semantically meaningful\\nresponse to confirm successful cancellation.\","]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"const\": \"cancelled\""]
#[doc = "    }"]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(
    :: serde :: Deserialize,
    :: serde :: Serialize,
    Clone,
    Copy,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
)]
pub enum StopReason {
    #[doc = "The turn ended successfully."]
    #[serde(rename = "end_turn")]
    EndTurn,
    #[doc = "The turn ended because the agent reached the maximum number of tokens."]
    #[serde(rename = "max_tokens")]
    MaxTokens,
    #[doc = "The turn ended because the agent reached the maximum number of allowed\nagent requests between user turns."]
    #[serde(rename = "max_turn_requests")]
    MaxTurnRequests,
    #[doc = "The turn ended because the agent refused to continue. The user prompt\nand everything that comes after it won't be included in the next\nprompt, so this should be reflected in the UI."]
    #[serde(rename = "refusal")]
    Refusal,
    #[doc = "The turn was cancelled by the client via `session/cancel`.\n\nThis stop reason MUST be returned when the client sends a `session/cancel`\nnotification, even if the cancellation causes exceptions in underlying operations.\nAgents should catch these exceptions and return this semantically meaningful\nresponse to confirm successful cancellation."]
    #[serde(rename = "cancelled")]
    Cancelled,
}
impl ::std::fmt::Display for StopReason {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        match *self {
            Self::EndTurn => f.write_str("end_turn"),
            Self::MaxTokens => f.write_str("max_tokens"),
            Self::MaxTurnRequests => f.write_str("max_turn_requests"),
            Self::Refusal => f.write_str("refusal"),
            Self::Cancelled => f.write_str("cancelled"),
        }
    }
}
impl ::std::str::FromStr for StopReason {
    type Err = self::error::ConversionError;
    fn from_str(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        match value {
            "end_turn" => Ok(Self::EndTurn),
            "max_tokens" => Ok(Self::MaxTokens),
            "max_turn_requests" => Ok(Self::MaxTurnRequests),
            "refusal" => Ok(Self::Refusal),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err("invalid value".into()),
        }
    }
}
impl ::std::convert::TryFrom<&str> for StopReason {
    type Error = self::error::ConversionError;
    fn try_from(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<&::std::string::String> for StopReason {
    type Error = self::error::ConversionError;
    fn try_from(
        value: &::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<::std::string::String> for StopReason {
    type Error = self::error::ConversionError;
    fn try_from(
        value: ::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
#[doc = "Embed a terminal created with `terminal/create` by its id.\n\nThe terminal must be added before calling `terminal/release`.\n\nSee protocol docs: [Terminal](https://agentclientprotocol.com/protocol/terminals)"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Embed a terminal created with `terminal/create` by its id.\\n\\nThe terminal must be added before calling `terminal/release`.\\n\\nSee protocol docs: [Terminal](https://agentclientprotocol.com/protocol/terminals)\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"terminalId\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"terminalId\": {"]
#[doc = "      \"description\": \"Identifier of the terminal instance to embed in the content stream.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/TerminalId\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct Terminal {
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "Identifier of the terminal instance to embed in the content stream."]
    #[serde(rename = "terminalId")]
    pub terminal_id: TerminalId,
}
impl Terminal {
    pub fn builder() -> builder::Terminal {
        Default::default()
    }
}
#[doc = "Exit status of a terminal command."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Exit status of a terminal command.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"exitCode\": {"]
#[doc = "      \"description\": \"The process exit code (may be null if terminated by signal).\","]
#[doc = "      \"type\": ["]
#[doc = "        \"integer\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"format\": \"uint32\","]
#[doc = "      \"minimum\": 0.0,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"signal\": {"]
#[doc = "      \"description\": \"The signal that terminated the process (may be null if exited normally).\","]
#[doc = "      \"type\": ["]
#[doc = "        \"string\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct TerminalExitStatus {
    #[doc = "The process exit code (may be null if terminated by signal)."]
    #[serde(
        rename = "exitCode",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub exit_code: ::std::option::Option<u32>,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "The signal that terminated the process (may be null if exited normally)."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub signal: ::std::option::Option<::std::string::String>,
}
impl ::std::default::Default for TerminalExitStatus {
    fn default() -> Self {
        Self {
            exit_code: Default::default(),
            meta: Default::default(),
            signal: Default::default(),
        }
    }
}
impl TerminalExitStatus {
    pub fn builder() -> builder::TerminalExitStatus {
        Default::default()
    }
}
#[doc = "Typed identifier used for terminal values on the wire."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Typed identifier used for terminal values on the wire.\","]
#[doc = "  \"type\": \"string\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(
    :: serde :: Deserialize,
    :: serde :: Serialize,
    Clone,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
)]
#[serde(transparent)]
pub struct TerminalId(pub ::std::string::String);
impl ::std::ops::Deref for TerminalId {
    type Target = ::std::string::String;
    fn deref(&self) -> &::std::string::String {
        &self.0
    }
}
impl ::std::convert::From<TerminalId> for ::std::string::String {
    fn from(value: TerminalId) -> Self {
        value.0
    }
}
impl ::std::convert::From<::std::string::String> for TerminalId {
    fn from(value: ::std::string::String) -> Self {
        Self(value)
    }
}
impl ::std::str::FromStr for TerminalId {
    type Err = ::std::convert::Infallible;
    fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
        Ok(Self(value.to_string()))
    }
}
impl ::std::fmt::Display for TerminalId {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        self.0.fmt(f)
    }
}
#[doc = "Request to get the current output and status of a terminal."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Request to get the current output and status of a terminal.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"sessionId\","]
#[doc = "    \"terminalId\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"sessionId\": {"]
#[doc = "      \"description\": \"The session ID for this request.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/SessionId\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    \"terminalId\": {"]
#[doc = "      \"description\": \"The ID of the terminal to get output from.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/TerminalId\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"x-method\": \"terminal/output\","]
#[doc = "  \"x-side\": \"client\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct TerminalOutputRequest {
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "The session ID for this request."]
    #[serde(rename = "sessionId")]
    pub session_id: SessionId,
    #[doc = "The ID of the terminal to get output from."]
    #[serde(rename = "terminalId")]
    pub terminal_id: TerminalId,
}
impl TerminalOutputRequest {
    pub fn builder() -> builder::TerminalOutputRequest {
        Default::default()
    }
}
#[doc = "Response containing the terminal output and exit status."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Response containing the terminal output and exit status.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"output\","]
#[doc = "    \"truncated\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"exitStatus\": {"]
#[doc = "      \"description\": \"Exit status if the command has completed.\","]
#[doc = "      \"anyOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/TerminalExitStatus\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"type\": \"null\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"output\": {"]
#[doc = "      \"description\": \"The terminal output captured so far.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"truncated\": {"]
#[doc = "      \"description\": \"Whether the output was truncated due to byte limits.\","]
#[doc = "      \"type\": \"boolean\""]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"x-method\": \"terminal/output\","]
#[doc = "  \"x-side\": \"client\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct TerminalOutputResponse {
    #[doc = "Exit status if the command has completed."]
    #[serde(
        rename = "exitStatus",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub exit_status: ::std::option::Option<TerminalExitStatus>,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "The terminal output captured so far."]
    pub output: ::std::string::String,
    #[doc = "Whether the output was truncated due to byte limits."]
    pub truncated: bool,
}
impl TerminalOutputResponse {
    pub fn builder() -> builder::TerminalOutputResponse {
        Default::default()
    }
}
#[doc = "Text provided to or from an LLM."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Text provided to or from an LLM.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"text\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"annotations\": {"]
#[doc = "      \"description\": \"Optional annotations that help clients decide how to display or route this content.\","]
#[doc = "      \"anyOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/Annotations\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"type\": \"null\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"text\": {"]
#[doc = "      \"description\": \"Text payload carried by this content block.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct TextContent {
    #[doc = "Optional annotations that help clients decide how to display or route this content."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub annotations: ::std::option::Option<Annotations>,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "Text payload carried by this content block."]
    pub text: ::std::string::String,
}
impl TextContent {
    pub fn builder() -> builder::TextContent {
        Default::default()
    }
}
#[doc = "Text-based resource contents."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Text-based resource contents.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"text\","]
#[doc = "    \"uri\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"mimeType\": {"]
#[doc = "      \"description\": \"MIME type describing the encoded media payload.\","]
#[doc = "      \"type\": ["]
#[doc = "        \"string\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"text\": {"]
#[doc = "      \"description\": \"Text payload carried by this content block.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"uri\": {"]
#[doc = "      \"description\": \"URI associated with this resource or media payload.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct TextResourceContents {
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "MIME type describing the encoded media payload."]
    #[serde(
        rename = "mimeType",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub mime_type: ::std::option::Option<::std::string::String>,
    #[doc = "Text payload carried by this content block."]
    pub text: ::std::string::String,
    #[doc = "URI associated with this resource or media payload."]
    pub uri: ::std::string::String,
}
impl TextResourceContents {
    pub fn builder() -> builder::TextResourceContents {
        Default::default()
    }
}
#[doc = "Represents a tool call that the language model has requested.\n\nTool calls are actions that the agent executes on behalf of the language model,\nsuch as reading files, executing code, or fetching data from external sources.\n\nSee protocol docs: [Tool Calls](https://agentclientprotocol.com/protocol/tool-calls)"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Represents a tool call that the language model has requested.\\n\\nTool calls are actions that the agent executes on behalf of the language model,\\nsuch as reading files, executing code, or fetching data from external sources.\\n\\nSee protocol docs: [Tool Calls](https://agentclientprotocol.com/protocol/tool-calls)\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"title\","]
#[doc = "    \"toolCallId\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"content\": {"]
#[doc = "      \"description\": \"Content produced by the tool call.\","]
#[doc = "      \"type\": \"array\","]
#[doc = "      \"items\": {"]
#[doc = "        \"$ref\": \"#/$defs/ToolCallContent\""]
#[doc = "      },"]
#[doc = "      \"x-deserialize-default-on-error\": true,"]
#[doc = "      \"x-deserialize-skip-invalid-items\": true"]
#[doc = "    },"]
#[doc = "    \"kind\": {"]
#[doc = "      \"description\": \"The category of tool being invoked.\\nHelps clients choose appropriate icons and UI treatment.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/ToolKind\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"locations\": {"]
#[doc = "      \"description\": \"File locations affected by this tool call.\\nEnables \\\"follow-along\\\" features in clients.\","]
#[doc = "      \"type\": \"array\","]
#[doc = "      \"items\": {"]
#[doc = "        \"$ref\": \"#/$defs/ToolCallLocation\""]
#[doc = "      },"]
#[doc = "      \"x-deserialize-default-on-error\": true,"]
#[doc = "      \"x-deserialize-skip-invalid-items\": true"]
#[doc = "    },"]
#[doc = "    \"rawInput\": {"]
#[doc = "      \"description\": \"Raw input parameters sent to the tool.\","]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"rawOutput\": {"]
#[doc = "      \"description\": \"Raw output returned by the tool.\","]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"status\": {"]
#[doc = "      \"description\": \"Current execution status of the tool call.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/ToolCallStatus\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"title\": {"]
#[doc = "      \"description\": \"Human-readable title describing what the tool is doing.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"toolCallId\": {"]
#[doc = "      \"description\": \"Unique identifier for this tool call within the session.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/ToolCallId\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct ToolCall {
    #[doc = "Content produced by the tool call."]
    #[serde(default, skip_serializing_if = "::std::vec::Vec::is_empty")]
    pub content: ::std::vec::Vec<ToolCallContent>,
    #[doc = "The category of tool being invoked.\nHelps clients choose appropriate icons and UI treatment."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub kind: ::std::option::Option<ToolKind>,
    #[doc = "File locations affected by this tool call.\nEnables \"follow-along\" features in clients."]
    #[serde(default, skip_serializing_if = "::std::vec::Vec::is_empty")]
    pub locations: ::std::vec::Vec<ToolCallLocation>,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "Raw input parameters sent to the tool."]
    #[serde(
        rename = "rawInput",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub raw_input: ::std::option::Option<::serde_json::Value>,
    #[doc = "Raw output returned by the tool."]
    #[serde(
        rename = "rawOutput",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub raw_output: ::std::option::Option<::serde_json::Value>,
    #[doc = "Current execution status of the tool call."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub status: ::std::option::Option<ToolCallStatus>,
    #[doc = "Human-readable title describing what the tool is doing."]
    pub title: ::std::string::String,
    #[doc = "Unique identifier for this tool call within the session."]
    #[serde(rename = "toolCallId")]
    pub tool_call_id: ToolCallId,
}
impl ToolCall {
    pub fn builder() -> builder::ToolCall {
        Default::default()
    }
}
#[doc = "Content produced by a tool call.\n\nTool calls can produce different types of content including\nstandard content blocks (text, images) or file diffs.\n\nSee protocol docs: [Content](https://agentclientprotocol.com/protocol/tool-calls#content)"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Content produced by a tool call.\\n\\nTool calls can produce different types of content including\\nstandard content blocks (text, images) or file diffs.\\n\\nSee protocol docs: [Content](https://agentclientprotocol.com/protocol/tool-calls#content)\","]
#[doc = "  \"oneOf\": ["]
#[doc = "    {"]
#[doc = "      \"description\": \"Standard content block (text, images, resources).\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/Content\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"required\": ["]
#[doc = "        \"type\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"type\": {"]
#[doc = "          \"type\": \"string\","]
#[doc = "          \"const\": \"content\""]
#[doc = "        }"]
#[doc = "      }"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"description\": \"File modification shown as a diff.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/Diff\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"required\": ["]
#[doc = "        \"type\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"type\": {"]
#[doc = "          \"type\": \"string\","]
#[doc = "          \"const\": \"diff\""]
#[doc = "        }"]
#[doc = "      }"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"description\": \"Embed a terminal created with `terminal/create` by its id.\\n\\nThe terminal must be added before calling `terminal/release`.\\n\\nSee protocol docs: [Terminal](https://agentclientprotocol.com/protocol/terminals)\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/Terminal\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"required\": ["]
#[doc = "        \"type\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"type\": {"]
#[doc = "          \"type\": \"string\","]
#[doc = "          \"const\": \"terminal\""]
#[doc = "        }"]
#[doc = "      }"]
#[doc = "    }"]
#[doc = "  ],"]
#[doc = "  \"discriminator\": {"]
#[doc = "    \"propertyName\": \"type\""]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
#[serde(untagged)]
pub enum ToolCallContent {
    Variant0 {
        #[doc = "The actual content block."]
        content: ContentBlock,
        #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
        #[serde(
            rename = "_meta",
            default,
            skip_serializing_if = "::std::option::Option::is_none"
        )]
        meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
        #[serde(rename = "type")]
        type_: ::std::string::String,
    },
    Variant1 {
        #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
        #[serde(
            rename = "_meta",
            default,
            skip_serializing_if = "::std::option::Option::is_none"
        )]
        meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
        #[doc = "The new content after modification."]
        #[serde(rename = "newText")]
        new_text: ::std::string::String,
        #[doc = "The original content (None for new files)."]
        #[serde(
            rename = "oldText",
            default,
            skip_serializing_if = "::std::option::Option::is_none"
        )]
        old_text: ::std::option::Option<::std::string::String>,
        #[doc = "The absolute file path being modified."]
        path: ::std::string::String,
        #[serde(rename = "type")]
        type_: ::std::string::String,
    },
    Variant2 {
        #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
        #[serde(
            rename = "_meta",
            default,
            skip_serializing_if = "::std::option::Option::is_none"
        )]
        meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
        #[doc = "Identifier of the terminal instance to embed in the content stream."]
        #[serde(rename = "terminalId")]
        terminal_id: TerminalId,
        #[serde(rename = "type")]
        type_: ::std::string::String,
    },
}
#[doc = "Unique identifier for a tool call within a session."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Unique identifier for a tool call within a session.\","]
#[doc = "  \"type\": \"string\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(
    :: serde :: Deserialize,
    :: serde :: Serialize,
    Clone,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
)]
#[serde(transparent)]
pub struct ToolCallId(pub ::std::string::String);
impl ::std::ops::Deref for ToolCallId {
    type Target = ::std::string::String;
    fn deref(&self) -> &::std::string::String {
        &self.0
    }
}
impl ::std::convert::From<ToolCallId> for ::std::string::String {
    fn from(value: ToolCallId) -> Self {
        value.0
    }
}
impl ::std::convert::From<::std::string::String> for ToolCallId {
    fn from(value: ::std::string::String) -> Self {
        Self(value)
    }
}
impl ::std::str::FromStr for ToolCallId {
    type Err = ::std::convert::Infallible;
    fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
        Ok(Self(value.to_string()))
    }
}
impl ::std::fmt::Display for ToolCallId {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        self.0.fmt(f)
    }
}
#[doc = "A file location being accessed or modified by a tool.\n\nEnables clients to implement \"follow-along\" features that track\nwhich files the agent is working with in real-time.\n\nSee protocol docs: [Following the Agent](https://agentclientprotocol.com/protocol/tool-calls#following-the-agent)"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"A file location being accessed or modified by a tool.\\n\\nEnables clients to implement \\\"follow-along\\\" features that track\\nwhich files the agent is working with in real-time.\\n\\nSee protocol docs: [Following the Agent](https://agentclientprotocol.com/protocol/tool-calls#following-the-agent)\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"path\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"line\": {"]
#[doc = "      \"description\": \"Optional line number within the file.\","]
#[doc = "      \"type\": ["]
#[doc = "        \"integer\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"format\": \"uint32\","]
#[doc = "      \"minimum\": 0.0,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"path\": {"]
#[doc = "      \"description\": \"The absolute file path being accessed or modified.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct ToolCallLocation {
    #[doc = "Optional line number within the file."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub line: ::std::option::Option<u32>,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "The absolute file path being accessed or modified."]
    pub path: ::std::string::String,
}
impl ToolCallLocation {
    pub fn builder() -> builder::ToolCallLocation {
        Default::default()
    }
}
#[doc = "Execution status of a tool call.\n\nTool calls progress through different statuses during their lifecycle.\n\nSee protocol docs: [Status](https://agentclientprotocol.com/protocol/tool-calls#status)"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Execution status of a tool call.\\n\\nTool calls progress through different statuses during their lifecycle.\\n\\nSee protocol docs: [Status](https://agentclientprotocol.com/protocol/tool-calls#status)\","]
#[doc = "  \"oneOf\": ["]
#[doc = "    {"]
#[doc = "      \"description\": \"The tool call hasn't started running yet because the input is either\\nstreaming or we're awaiting approval.\","]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"const\": \"pending\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"description\": \"The tool call is currently running.\","]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"const\": \"in_progress\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"description\": \"The tool call completed successfully.\","]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"const\": \"completed\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"description\": \"The tool call failed with an error.\","]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"const\": \"failed\""]
#[doc = "    }"]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(
    :: serde :: Deserialize,
    :: serde :: Serialize,
    Clone,
    Copy,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
)]
pub enum ToolCallStatus {
    #[doc = "The tool call hasn't started running yet because the input is either\nstreaming or we're awaiting approval."]
    #[serde(rename = "pending")]
    Pending,
    #[doc = "The tool call is currently running."]
    #[serde(rename = "in_progress")]
    InProgress,
    #[doc = "The tool call completed successfully."]
    #[serde(rename = "completed")]
    Completed,
    #[doc = "The tool call failed with an error."]
    #[serde(rename = "failed")]
    Failed,
}
impl ::std::fmt::Display for ToolCallStatus {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        match *self {
            Self::Pending => f.write_str("pending"),
            Self::InProgress => f.write_str("in_progress"),
            Self::Completed => f.write_str("completed"),
            Self::Failed => f.write_str("failed"),
        }
    }
}
impl ::std::str::FromStr for ToolCallStatus {
    type Err = self::error::ConversionError;
    fn from_str(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        match value {
            "pending" => Ok(Self::Pending),
            "in_progress" => Ok(Self::InProgress),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            _ => Err("invalid value".into()),
        }
    }
}
impl ::std::convert::TryFrom<&str> for ToolCallStatus {
    type Error = self::error::ConversionError;
    fn try_from(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<&::std::string::String> for ToolCallStatus {
    type Error = self::error::ConversionError;
    fn try_from(
        value: &::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<::std::string::String> for ToolCallStatus {
    type Error = self::error::ConversionError;
    fn try_from(
        value: ::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
#[doc = "An update to an existing tool call.\n\nUsed to report progress and results as tools execute. All fields except\nthe tool call ID are optional - only changed fields need to be included.\n\nSee protocol docs: [Updating](https://agentclientprotocol.com/protocol/tool-calls#updating)"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"An update to an existing tool call.\\n\\nUsed to report progress and results as tools execute. All fields except\\nthe tool call ID are optional - only changed fields need to be included.\\n\\nSee protocol docs: [Updating](https://agentclientprotocol.com/protocol/tool-calls#updating)\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"toolCallId\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"content\": {"]
#[doc = "      \"description\": \"Replace the content collection.\","]
#[doc = "      \"type\": ["]
#[doc = "        \"array\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"items\": {"]
#[doc = "        \"$ref\": \"#/$defs/ToolCallContent\""]
#[doc = "      },"]
#[doc = "      \"x-deserialize-default-on-error\": true,"]
#[doc = "      \"x-deserialize-skip-invalid-items\": true"]
#[doc = "    },"]
#[doc = "    \"kind\": {"]
#[doc = "      \"description\": \"Update the tool kind.\","]
#[doc = "      \"anyOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/ToolKind\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"type\": \"null\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"locations\": {"]
#[doc = "      \"description\": \"Replace the locations collection.\","]
#[doc = "      \"type\": ["]
#[doc = "        \"array\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"items\": {"]
#[doc = "        \"$ref\": \"#/$defs/ToolCallLocation\""]
#[doc = "      },"]
#[doc = "      \"x-deserialize-default-on-error\": true,"]
#[doc = "      \"x-deserialize-skip-invalid-items\": true"]
#[doc = "    },"]
#[doc = "    \"rawInput\": {"]
#[doc = "      \"description\": \"Update the raw input.\","]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"rawOutput\": {"]
#[doc = "      \"description\": \"Update the raw output.\","]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"status\": {"]
#[doc = "      \"description\": \"Update the execution status.\","]
#[doc = "      \"anyOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/ToolCallStatus\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"type\": \"null\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"title\": {"]
#[doc = "      \"description\": \"Update the human-readable title.\","]
#[doc = "      \"type\": ["]
#[doc = "        \"string\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"toolCallId\": {"]
#[doc = "      \"description\": \"The ID of the tool call being updated.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/ToolCallId\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct ToolCallUpdate {
    #[doc = "Replace the content collection."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub content: ::std::option::Option<::std::vec::Vec<ToolCallContent>>,
    #[doc = "Update the tool kind."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub kind: ::std::option::Option<ToolKind>,
    #[doc = "Replace the locations collection."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub locations: ::std::option::Option<::std::vec::Vec<ToolCallLocation>>,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "Update the raw input."]
    #[serde(
        rename = "rawInput",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub raw_input: ::std::option::Option<::serde_json::Value>,
    #[doc = "Update the raw output."]
    #[serde(
        rename = "rawOutput",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub raw_output: ::std::option::Option<::serde_json::Value>,
    #[doc = "Update the execution status."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub status: ::std::option::Option<ToolCallStatus>,
    #[doc = "Update the human-readable title."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub title: ::std::option::Option<::std::string::String>,
    #[doc = "The ID of the tool call being updated."]
    #[serde(rename = "toolCallId")]
    pub tool_call_id: ToolCallId,
}
impl ToolCallUpdate {
    pub fn builder() -> builder::ToolCallUpdate {
        Default::default()
    }
}
#[doc = "Categories of tools that can be invoked.\n\nTool kinds help clients choose appropriate icons and optimize how they\ndisplay tool execution progress.\n\nSee protocol docs: [Creating](https://agentclientprotocol.com/protocol/tool-calls#creating)"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Categories of tools that can be invoked.\\n\\nTool kinds help clients choose appropriate icons and optimize how they\\ndisplay tool execution progress.\\n\\nSee protocol docs: [Creating](https://agentclientprotocol.com/protocol/tool-calls#creating)\","]
#[doc = "  \"oneOf\": ["]
#[doc = "    {"]
#[doc = "      \"description\": \"Reading files or data.\","]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"const\": \"read\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"description\": \"Modifying files or content.\","]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"const\": \"edit\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"description\": \"Removing files or data.\","]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"const\": \"delete\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"description\": \"Moving or renaming files.\","]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"const\": \"move\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"description\": \"Searching for information.\","]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"const\": \"search\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"description\": \"Running commands or code.\","]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"const\": \"execute\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"description\": \"Internal reasoning or planning.\","]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"const\": \"think\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"description\": \"Retrieving external data.\","]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"const\": \"fetch\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"description\": \"Switching the current session mode.\","]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"const\": \"switch_mode\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"description\": \"Other tool types (default).\","]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"const\": \"other\""]
#[doc = "    }"]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(
    :: serde :: Deserialize,
    :: serde :: Serialize,
    Clone,
    Copy,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
)]
pub enum ToolKind {
    #[doc = "Reading files or data."]
    #[serde(rename = "read")]
    Read,
    #[doc = "Modifying files or content."]
    #[serde(rename = "edit")]
    Edit,
    #[doc = "Removing files or data."]
    #[serde(rename = "delete")]
    Delete,
    #[doc = "Moving or renaming files."]
    #[serde(rename = "move")]
    Move,
    #[doc = "Searching for information."]
    #[serde(rename = "search")]
    Search,
    #[doc = "Running commands or code."]
    #[serde(rename = "execute")]
    Execute,
    #[doc = "Internal reasoning or planning."]
    #[serde(rename = "think")]
    Think,
    #[doc = "Retrieving external data."]
    #[serde(rename = "fetch")]
    Fetch,
    #[doc = "Switching the current session mode."]
    #[serde(rename = "switch_mode")]
    SwitchMode,
    #[doc = "Other tool types (default)."]
    #[serde(rename = "other")]
    Other,
}
impl ::std::fmt::Display for ToolKind {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        match *self {
            Self::Read => f.write_str("read"),
            Self::Edit => f.write_str("edit"),
            Self::Delete => f.write_str("delete"),
            Self::Move => f.write_str("move"),
            Self::Search => f.write_str("search"),
            Self::Execute => f.write_str("execute"),
            Self::Think => f.write_str("think"),
            Self::Fetch => f.write_str("fetch"),
            Self::SwitchMode => f.write_str("switch_mode"),
            Self::Other => f.write_str("other"),
        }
    }
}
impl ::std::str::FromStr for ToolKind {
    type Err = self::error::ConversionError;
    fn from_str(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        match value {
            "read" => Ok(Self::Read),
            "edit" => Ok(Self::Edit),
            "delete" => Ok(Self::Delete),
            "move" => Ok(Self::Move),
            "search" => Ok(Self::Search),
            "execute" => Ok(Self::Execute),
            "think" => Ok(Self::Think),
            "fetch" => Ok(Self::Fetch),
            "switch_mode" => Ok(Self::SwitchMode),
            "other" => Ok(Self::Other),
            _ => Err("invalid value".into()),
        }
    }
}
impl ::std::convert::TryFrom<&str> for ToolKind {
    type Error = self::error::ConversionError;
    fn try_from(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<&::std::string::String> for ToolKind {
    type Error = self::error::ConversionError;
    fn try_from(
        value: &::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<::std::string::String> for ToolKind {
    type Error = self::error::ConversionError;
    fn try_from(
        value: ::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
#[doc = "All text that was typed after the command name is provided as input."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"All text that was typed after the command name is provided as input.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"hint\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"hint\": {"]
#[doc = "      \"description\": \"A hint to display when the input hasn't been provided yet\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct UnstructuredCommandInput {
    #[doc = "A hint to display when the input hasn't been provided yet"]
    pub hint: ::std::string::String,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
}
impl UnstructuredCommandInput {
    pub fn builder() -> builder::UnstructuredCommandInput {
        Default::default()
    }
}
#[doc = "Context window and cost update for a session."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Context window and cost update for a session.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"size\","]
#[doc = "    \"used\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"cost\": {"]
#[doc = "      \"description\": \"Cumulative session cost (optional).\","]
#[doc = "      \"anyOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/Cost\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"type\": \"null\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"size\": {"]
#[doc = "      \"description\": \"Total context window size in tokens.\","]
#[doc = "      \"type\": \"integer\","]
#[doc = "      \"format\": \"uint64\","]
#[doc = "      \"minimum\": 0.0"]
#[doc = "    },"]
#[doc = "    \"used\": {"]
#[doc = "      \"description\": \"Tokens currently in context.\","]
#[doc = "      \"type\": \"integer\","]
#[doc = "      \"format\": \"uint64\","]
#[doc = "      \"minimum\": 0.0"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct UsageUpdate {
    #[doc = "Cumulative session cost (optional)."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub cost: ::std::option::Option<Cost>,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "Total context window size in tokens."]
    pub size: u64,
    #[doc = "Tokens currently in context."]
    pub used: u64,
}
impl UsageUpdate {
    pub fn builder() -> builder::UsageUpdate {
        Default::default()
    }
}
#[doc = "Request to wait for a terminal command to exit."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Request to wait for a terminal command to exit.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"sessionId\","]
#[doc = "    \"terminalId\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"sessionId\": {"]
#[doc = "      \"description\": \"The session ID for this request.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/SessionId\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    \"terminalId\": {"]
#[doc = "      \"description\": \"The ID of the terminal to wait for.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/TerminalId\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"x-method\": \"terminal/wait_for_exit\","]
#[doc = "  \"x-side\": \"client\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct WaitForTerminalExitRequest {
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "The session ID for this request."]
    #[serde(rename = "sessionId")]
    pub session_id: SessionId,
    #[doc = "The ID of the terminal to wait for."]
    #[serde(rename = "terminalId")]
    pub terminal_id: TerminalId,
}
impl WaitForTerminalExitRequest {
    pub fn builder() -> builder::WaitForTerminalExitRequest {
        Default::default()
    }
}
#[doc = "Response containing the exit status of a terminal command."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Response containing the exit status of a terminal command.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"exitCode\": {"]
#[doc = "      \"description\": \"The process exit code (may be null if terminated by signal).\","]
#[doc = "      \"type\": ["]
#[doc = "        \"integer\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"format\": \"uint32\","]
#[doc = "      \"minimum\": 0.0,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"signal\": {"]
#[doc = "      \"description\": \"The signal that terminated the process (may be null if exited normally).\","]
#[doc = "      \"type\": ["]
#[doc = "        \"string\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"x-method\": \"terminal/wait_for_exit\","]
#[doc = "  \"x-side\": \"client\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct WaitForTerminalExitResponse {
    #[doc = "The process exit code (may be null if terminated by signal)."]
    #[serde(
        rename = "exitCode",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub exit_code: ::std::option::Option<u32>,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "The signal that terminated the process (may be null if exited normally)."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub signal: ::std::option::Option<::std::string::String>,
}
impl ::std::default::Default for WaitForTerminalExitResponse {
    fn default() -> Self {
        Self {
            exit_code: Default::default(),
            meta: Default::default(),
            signal: Default::default(),
        }
    }
}
impl WaitForTerminalExitResponse {
    pub fn builder() -> builder::WaitForTerminalExitResponse {
        Default::default()
    }
}
#[doc = "Request to write content to a text file.\n\nOnly available if the client supports the `fs.writeTextFile` capability."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Request to write content to a text file.\\n\\nOnly available if the client supports the `fs.writeTextFile` capability.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"content\","]
#[doc = "    \"path\","]
#[doc = "    \"sessionId\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    },"]
#[doc = "    \"content\": {"]
#[doc = "      \"description\": \"The text content to write to the file.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"path\": {"]
#[doc = "      \"description\": \"Absolute path to the file to write.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"sessionId\": {"]
#[doc = "      \"description\": \"The session ID for this request.\","]
#[doc = "      \"allOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/SessionId\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"x-method\": \"fs/write_text_file\","]
#[doc = "  \"x-side\": \"client\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct WriteTextFileRequest {
    #[doc = "The text content to write to the file."]
    pub content: ::std::string::String,
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
    #[doc = "Absolute path to the file to write."]
    pub path: ::std::string::String,
    #[doc = "The session ID for this request."]
    #[serde(rename = "sessionId")]
    pub session_id: SessionId,
}
impl WriteTextFileRequest {
    pub fn builder() -> builder::WriteTextFileRequest {
        Default::default()
    }
}
#[doc = "Response to `fs/write_text_file`"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Response to `fs/write_text_file`\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"properties\": {"]
#[doc = "    \"_meta\": {"]
#[doc = "      \"description\": \"The _meta property is reserved by ACP to allow clients and agents to attach additional\\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\\nthese keys.\\n\\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)\","]
#[doc = "      \"type\": ["]
#[doc = "        \"object\","]
#[doc = "        \"null\""]
#[doc = "      ],"]
#[doc = "      \"additionalProperties\": true,"]
#[doc = "      \"x-deserialize-default-on-error\": true"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"x-method\": \"fs/write_text_file\","]
#[doc = "  \"x-side\": \"client\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct WriteTextFileResponse {
    #[doc = "The _meta property is reserved by ACP to allow clients and agents to attach additional\nmetadata to their interactions. Implementations MUST NOT make assumptions about values at\nthese keys.\n\nSee protocol docs: [Extensibility](https://agentclientprotocol.com/protocol/extensibility)"]
    #[serde(
        rename = "_meta",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub meta: ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
}
impl ::std::default::Default for WriteTextFileResponse {
    fn default() -> Self {
        Self {
            meta: Default::default(),
        }
    }
}
impl WriteTextFileResponse {
    pub fn builder() -> builder::WriteTextFileResponse {
        Default::default()
    }
}
#[doc = r" Types for composing complex structures."]
pub mod builder {
    #[derive(Clone, Debug)]
    pub struct AgentAuthCapabilities {
        logout: ::std::result::Result<
            ::std::option::Option<super::LogoutCapabilities>,
            ::std::string::String,
        >,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for AgentAuthCapabilities {
        fn default() -> Self {
            Self {
                logout: Ok(Default::default()),
                meta: Ok(Default::default()),
            }
        }
    }
    impl AgentAuthCapabilities {
        pub fn logout<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::LogoutCapabilities>>,
            T::Error: ::std::fmt::Display,
        {
            self.logout = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for logout: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<AgentAuthCapabilities> for super::AgentAuthCapabilities {
        type Error = super::error::ConversionError;
        fn try_from(
            value: AgentAuthCapabilities,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                logout: value.logout?,
                meta: value.meta?,
            })
        }
    }
    impl ::std::convert::From<super::AgentAuthCapabilities> for AgentAuthCapabilities {
        fn from(value: super::AgentAuthCapabilities) -> Self {
            Self {
                logout: Ok(value.logout),
                meta: Ok(value.meta),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct AgentCapabilities {
        auth: ::std::result::Result<super::AgentAuthCapabilities, ::std::string::String>,
        load_session: ::std::result::Result<bool, ::std::string::String>,
        mcp_capabilities: ::std::result::Result<super::McpCapabilities, ::std::string::String>,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        prompt_capabilities:
            ::std::result::Result<super::PromptCapabilities, ::std::string::String>,
        session_capabilities:
            ::std::result::Result<super::SessionCapabilities, ::std::string::String>,
    }
    impl ::std::default::Default for AgentCapabilities {
        fn default() -> Self {
            Self {
                auth: Ok(super::defaults::agent_capabilities_auth()),
                load_session: Ok(Default::default()),
                mcp_capabilities: Ok(super::defaults::agent_capabilities_mcp_capabilities()),
                meta: Ok(Default::default()),
                prompt_capabilities: Ok(super::defaults::agent_capabilities_prompt_capabilities()),
                session_capabilities: Ok(super::defaults::agent_capabilities_session_capabilities()),
            }
        }
    }
    impl AgentCapabilities {
        pub fn auth<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::AgentAuthCapabilities>,
            T::Error: ::std::fmt::Display,
        {
            self.auth = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for auth: {e}"));
            self
        }
        pub fn load_session<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<bool>,
            T::Error: ::std::fmt::Display,
        {
            self.load_session = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for load_session: {e}"));
            self
        }
        pub fn mcp_capabilities<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::McpCapabilities>,
            T::Error: ::std::fmt::Display,
        {
            self.mcp_capabilities = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for mcp_capabilities: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn prompt_capabilities<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::PromptCapabilities>,
            T::Error: ::std::fmt::Display,
        {
            self.prompt_capabilities = value.try_into().map_err(|e| {
                format!("error converting supplied value for prompt_capabilities: {e}")
            });
            self
        }
        pub fn session_capabilities<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::SessionCapabilities>,
            T::Error: ::std::fmt::Display,
        {
            self.session_capabilities = value.try_into().map_err(|e| {
                format!("error converting supplied value for session_capabilities: {e}")
            });
            self
        }
    }
    impl ::std::convert::TryFrom<AgentCapabilities> for super::AgentCapabilities {
        type Error = super::error::ConversionError;
        fn try_from(
            value: AgentCapabilities,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                auth: value.auth?,
                load_session: value.load_session?,
                mcp_capabilities: value.mcp_capabilities?,
                meta: value.meta?,
                prompt_capabilities: value.prompt_capabilities?,
                session_capabilities: value.session_capabilities?,
            })
        }
    }
    impl ::std::convert::From<super::AgentCapabilities> for AgentCapabilities {
        fn from(value: super::AgentCapabilities) -> Self {
            Self {
                auth: Ok(value.auth),
                load_session: Ok(value.load_session),
                mcp_capabilities: Ok(value.mcp_capabilities),
                meta: Ok(value.meta),
                prompt_capabilities: Ok(value.prompt_capabilities),
                session_capabilities: Ok(value.session_capabilities),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct AgentClientProtocol {
        subtype_0:
            ::std::result::Result<::std::option::Option<super::Agent>, ::std::string::String>,
        subtype_1:
            ::std::result::Result<::std::option::Option<super::Client>, ::std::string::String>,
        subtype_2: ::std::result::Result<
            ::std::option::Option<super::ProtocolLevel>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for AgentClientProtocol {
        fn default() -> Self {
            Self {
                subtype_0: Ok(Default::default()),
                subtype_1: Ok(Default::default()),
                subtype_2: Ok(Default::default()),
            }
        }
    }
    impl AgentClientProtocol {
        pub fn subtype_0<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::Agent>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_0 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_0: {e}"));
            self
        }
        pub fn subtype_1<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::Client>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_1 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_1: {e}"));
            self
        }
        pub fn subtype_2<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::ProtocolLevel>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_2 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_2: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<AgentClientProtocol> for super::AgentClientProtocol {
        type Error = super::error::ConversionError;
        fn try_from(
            value: AgentClientProtocol,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                subtype_0: value.subtype_0?,
                subtype_1: value.subtype_1?,
                subtype_2: value.subtype_2?,
            })
        }
    }
    impl ::std::convert::From<super::AgentClientProtocol> for AgentClientProtocol {
        fn from(value: super::AgentClientProtocol) -> Self {
            Self {
                subtype_0: Ok(value.subtype_0),
                subtype_1: Ok(value.subtype_1),
                subtype_2: Ok(value.subtype_2),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct AgentNotification {
        method: ::std::result::Result<::std::string::String, ::std::string::String>,
        params: ::std::result::Result<
            ::std::option::Option<super::AgentNotificationParams>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for AgentNotification {
        fn default() -> Self {
            Self {
                method: Err("no value supplied for method".to_string()),
                params: Ok(Default::default()),
            }
        }
    }
    impl AgentNotification {
        pub fn method<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.method = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for method: {e}"));
            self
        }
        pub fn params<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::AgentNotificationParams>>,
            T::Error: ::std::fmt::Display,
        {
            self.params = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for params: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<AgentNotification> for super::AgentNotification {
        type Error = super::error::ConversionError;
        fn try_from(
            value: AgentNotification,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                method: value.method?,
                params: value.params?,
            })
        }
    }
    impl ::std::convert::From<super::AgentNotification> for AgentNotification {
        fn from(value: super::AgentNotification) -> Self {
            Self {
                method: Ok(value.method),
                params: Ok(value.params),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct AgentNotificationParams {
        subtype_0: ::std::result::Result<
            ::std::option::Option<super::SessionNotification>,
            ::std::string::String,
        >,
        subtype_1: ::std::result::Result<
            ::std::option::Option<super::ExtNotification>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for AgentNotificationParams {
        fn default() -> Self {
            Self {
                subtype_0: Ok(Default::default()),
                subtype_1: Ok(Default::default()),
            }
        }
    }
    impl AgentNotificationParams {
        pub fn subtype_0<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::SessionNotification>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_0 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_0: {e}"));
            self
        }
        pub fn subtype_1<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::ExtNotification>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_1 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_1: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<AgentNotificationParams> for super::AgentNotificationParams {
        type Error = super::error::ConversionError;
        fn try_from(
            value: AgentNotificationParams,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                subtype_0: value.subtype_0?,
                subtype_1: value.subtype_1?,
            })
        }
    }
    impl ::std::convert::From<super::AgentNotificationParams> for AgentNotificationParams {
        fn from(value: super::AgentNotificationParams) -> Self {
            Self {
                subtype_0: Ok(value.subtype_0),
                subtype_1: Ok(value.subtype_1),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct AgentRequest {
        id: ::std::result::Result<super::RequestId, ::std::string::String>,
        method: ::std::result::Result<::std::string::String, ::std::string::String>,
        params: ::std::result::Result<
            ::std::option::Option<super::AgentRequestParams>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for AgentRequest {
        fn default() -> Self {
            Self {
                id: Err("no value supplied for id".to_string()),
                method: Err("no value supplied for method".to_string()),
                params: Ok(Default::default()),
            }
        }
    }
    impl AgentRequest {
        pub fn id<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::RequestId>,
            T::Error: ::std::fmt::Display,
        {
            self.id = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for id: {e}"));
            self
        }
        pub fn method<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.method = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for method: {e}"));
            self
        }
        pub fn params<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::AgentRequestParams>>,
            T::Error: ::std::fmt::Display,
        {
            self.params = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for params: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<AgentRequest> for super::AgentRequest {
        type Error = super::error::ConversionError;
        fn try_from(
            value: AgentRequest,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                id: value.id?,
                method: value.method?,
                params: value.params?,
            })
        }
    }
    impl ::std::convert::From<super::AgentRequest> for AgentRequest {
        fn from(value: super::AgentRequest) -> Self {
            Self {
                id: Ok(value.id),
                method: Ok(value.method),
                params: Ok(value.params),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct AgentRequestParams {
        subtype_0: ::std::result::Result<
            ::std::option::Option<super::WriteTextFileRequest>,
            ::std::string::String,
        >,
        subtype_1: ::std::result::Result<
            ::std::option::Option<super::ReadTextFileRequest>,
            ::std::string::String,
        >,
        subtype_2: ::std::result::Result<
            ::std::option::Option<super::RequestPermissionRequest>,
            ::std::string::String,
        >,
        subtype_3: ::std::result::Result<
            ::std::option::Option<super::CreateTerminalRequest>,
            ::std::string::String,
        >,
        subtype_4: ::std::result::Result<
            ::std::option::Option<super::TerminalOutputRequest>,
            ::std::string::String,
        >,
        subtype_5: ::std::result::Result<
            ::std::option::Option<super::ReleaseTerminalRequest>,
            ::std::string::String,
        >,
        subtype_6: ::std::result::Result<
            ::std::option::Option<super::WaitForTerminalExitRequest>,
            ::std::string::String,
        >,
        subtype_7: ::std::result::Result<
            ::std::option::Option<super::KillTerminalRequest>,
            ::std::string::String,
        >,
        subtype_8:
            ::std::result::Result<::std::option::Option<super::ExtRequest>, ::std::string::String>,
    }
    impl ::std::default::Default for AgentRequestParams {
        fn default() -> Self {
            Self {
                subtype_0: Ok(Default::default()),
                subtype_1: Ok(Default::default()),
                subtype_2: Ok(Default::default()),
                subtype_3: Ok(Default::default()),
                subtype_4: Ok(Default::default()),
                subtype_5: Ok(Default::default()),
                subtype_6: Ok(Default::default()),
                subtype_7: Ok(Default::default()),
                subtype_8: Ok(Default::default()),
            }
        }
    }
    impl AgentRequestParams {
        pub fn subtype_0<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::WriteTextFileRequest>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_0 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_0: {e}"));
            self
        }
        pub fn subtype_1<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::ReadTextFileRequest>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_1 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_1: {e}"));
            self
        }
        pub fn subtype_2<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::RequestPermissionRequest>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_2 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_2: {e}"));
            self
        }
        pub fn subtype_3<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::CreateTerminalRequest>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_3 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_3: {e}"));
            self
        }
        pub fn subtype_4<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::TerminalOutputRequest>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_4 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_4: {e}"));
            self
        }
        pub fn subtype_5<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::ReleaseTerminalRequest>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_5 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_5: {e}"));
            self
        }
        pub fn subtype_6<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::WaitForTerminalExitRequest>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_6 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_6: {e}"));
            self
        }
        pub fn subtype_7<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::KillTerminalRequest>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_7 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_7: {e}"));
            self
        }
        pub fn subtype_8<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::ExtRequest>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_8 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_8: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<AgentRequestParams> for super::AgentRequestParams {
        type Error = super::error::ConversionError;
        fn try_from(
            value: AgentRequestParams,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                subtype_0: value.subtype_0?,
                subtype_1: value.subtype_1?,
                subtype_2: value.subtype_2?,
                subtype_3: value.subtype_3?,
                subtype_4: value.subtype_4?,
                subtype_5: value.subtype_5?,
                subtype_6: value.subtype_6?,
                subtype_7: value.subtype_7?,
                subtype_8: value.subtype_8?,
            })
        }
    }
    impl ::std::convert::From<super::AgentRequestParams> for AgentRequestParams {
        fn from(value: super::AgentRequestParams) -> Self {
            Self {
                subtype_0: Ok(value.subtype_0),
                subtype_1: Ok(value.subtype_1),
                subtype_2: Ok(value.subtype_2),
                subtype_3: Ok(value.subtype_3),
                subtype_4: Ok(value.subtype_4),
                subtype_5: Ok(value.subtype_5),
                subtype_6: Ok(value.subtype_6),
                subtype_7: Ok(value.subtype_7),
                subtype_8: Ok(value.subtype_8),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct AgentVariant0Params {
        subtype_0: ::std::result::Result<
            ::std::option::Option<super::WriteTextFileRequest>,
            ::std::string::String,
        >,
        subtype_1: ::std::result::Result<
            ::std::option::Option<super::ReadTextFileRequest>,
            ::std::string::String,
        >,
        subtype_2: ::std::result::Result<
            ::std::option::Option<super::RequestPermissionRequest>,
            ::std::string::String,
        >,
        subtype_3: ::std::result::Result<
            ::std::option::Option<super::CreateTerminalRequest>,
            ::std::string::String,
        >,
        subtype_4: ::std::result::Result<
            ::std::option::Option<super::TerminalOutputRequest>,
            ::std::string::String,
        >,
        subtype_5: ::std::result::Result<
            ::std::option::Option<super::ReleaseTerminalRequest>,
            ::std::string::String,
        >,
        subtype_6: ::std::result::Result<
            ::std::option::Option<super::WaitForTerminalExitRequest>,
            ::std::string::String,
        >,
        subtype_7: ::std::result::Result<
            ::std::option::Option<super::KillTerminalRequest>,
            ::std::string::String,
        >,
        subtype_8:
            ::std::result::Result<::std::option::Option<super::ExtRequest>, ::std::string::String>,
    }
    impl ::std::default::Default for AgentVariant0Params {
        fn default() -> Self {
            Self {
                subtype_0: Ok(Default::default()),
                subtype_1: Ok(Default::default()),
                subtype_2: Ok(Default::default()),
                subtype_3: Ok(Default::default()),
                subtype_4: Ok(Default::default()),
                subtype_5: Ok(Default::default()),
                subtype_6: Ok(Default::default()),
                subtype_7: Ok(Default::default()),
                subtype_8: Ok(Default::default()),
            }
        }
    }
    impl AgentVariant0Params {
        pub fn subtype_0<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::WriteTextFileRequest>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_0 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_0: {e}"));
            self
        }
        pub fn subtype_1<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::ReadTextFileRequest>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_1 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_1: {e}"));
            self
        }
        pub fn subtype_2<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::RequestPermissionRequest>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_2 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_2: {e}"));
            self
        }
        pub fn subtype_3<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::CreateTerminalRequest>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_3 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_3: {e}"));
            self
        }
        pub fn subtype_4<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::TerminalOutputRequest>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_4 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_4: {e}"));
            self
        }
        pub fn subtype_5<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::ReleaseTerminalRequest>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_5 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_5: {e}"));
            self
        }
        pub fn subtype_6<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::WaitForTerminalExitRequest>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_6 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_6: {e}"));
            self
        }
        pub fn subtype_7<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::KillTerminalRequest>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_7 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_7: {e}"));
            self
        }
        pub fn subtype_8<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::ExtRequest>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_8 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_8: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<AgentVariant0Params> for super::AgentVariant0Params {
        type Error = super::error::ConversionError;
        fn try_from(
            value: AgentVariant0Params,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                subtype_0: value.subtype_0?,
                subtype_1: value.subtype_1?,
                subtype_2: value.subtype_2?,
                subtype_3: value.subtype_3?,
                subtype_4: value.subtype_4?,
                subtype_5: value.subtype_5?,
                subtype_6: value.subtype_6?,
                subtype_7: value.subtype_7?,
                subtype_8: value.subtype_8?,
            })
        }
    }
    impl ::std::convert::From<super::AgentVariant0Params> for AgentVariant0Params {
        fn from(value: super::AgentVariant0Params) -> Self {
            Self {
                subtype_0: Ok(value.subtype_0),
                subtype_1: Ok(value.subtype_1),
                subtype_2: Ok(value.subtype_2),
                subtype_3: Ok(value.subtype_3),
                subtype_4: Ok(value.subtype_4),
                subtype_5: Ok(value.subtype_5),
                subtype_6: Ok(value.subtype_6),
                subtype_7: Ok(value.subtype_7),
                subtype_8: Ok(value.subtype_8),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct AgentVariant2Params {
        subtype_0: ::std::result::Result<
            ::std::option::Option<super::SessionNotification>,
            ::std::string::String,
        >,
        subtype_1: ::std::result::Result<
            ::std::option::Option<super::ExtNotification>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for AgentVariant2Params {
        fn default() -> Self {
            Self {
                subtype_0: Ok(Default::default()),
                subtype_1: Ok(Default::default()),
            }
        }
    }
    impl AgentVariant2Params {
        pub fn subtype_0<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::SessionNotification>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_0 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_0: {e}"));
            self
        }
        pub fn subtype_1<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::ExtNotification>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_1 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_1: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<AgentVariant2Params> for super::AgentVariant2Params {
        type Error = super::error::ConversionError;
        fn try_from(
            value: AgentVariant2Params,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                subtype_0: value.subtype_0?,
                subtype_1: value.subtype_1?,
            })
        }
    }
    impl ::std::convert::From<super::AgentVariant2Params> for AgentVariant2Params {
        fn from(value: super::AgentVariant2Params) -> Self {
            Self {
                subtype_0: Ok(value.subtype_0),
                subtype_1: Ok(value.subtype_1),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct Annotations {
        audience: ::std::result::Result<
            ::std::option::Option<::std::vec::Vec<super::Role>>,
            ::std::string::String,
        >,
        last_modified: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        priority: ::std::result::Result<::std::option::Option<f64>, ::std::string::String>,
    }
    impl ::std::default::Default for Annotations {
        fn default() -> Self {
            Self {
                audience: Ok(Default::default()),
                last_modified: Ok(Default::default()),
                meta: Ok(Default::default()),
                priority: Ok(Default::default()),
            }
        }
    }
    impl Annotations {
        pub fn audience<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::vec::Vec<super::Role>>>,
            T::Error: ::std::fmt::Display,
        {
            self.audience = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for audience: {e}"));
            self
        }
        pub fn last_modified<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.last_modified = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for last_modified: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn priority<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<f64>>,
            T::Error: ::std::fmt::Display,
        {
            self.priority = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for priority: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<Annotations> for super::Annotations {
        type Error = super::error::ConversionError;
        fn try_from(
            value: Annotations,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                audience: value.audience?,
                last_modified: value.last_modified?,
                meta: value.meta?,
                priority: value.priority?,
            })
        }
    }
    impl ::std::convert::From<super::Annotations> for Annotations {
        fn from(value: super::Annotations) -> Self {
            Self {
                audience: Ok(value.audience),
                last_modified: Ok(value.last_modified),
                meta: Ok(value.meta),
                priority: Ok(value.priority),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct AudioContent {
        annotations:
            ::std::result::Result<::std::option::Option<super::Annotations>, ::std::string::String>,
        data: ::std::result::Result<::std::string::String, ::std::string::String>,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        mime_type: ::std::result::Result<::std::string::String, ::std::string::String>,
    }
    impl ::std::default::Default for AudioContent {
        fn default() -> Self {
            Self {
                annotations: Ok(Default::default()),
                data: Err("no value supplied for data".to_string()),
                meta: Ok(Default::default()),
                mime_type: Err("no value supplied for mime_type".to_string()),
            }
        }
    }
    impl AudioContent {
        pub fn annotations<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::Annotations>>,
            T::Error: ::std::fmt::Display,
        {
            self.annotations = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for annotations: {e}"));
            self
        }
        pub fn data<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.data = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for data: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn mime_type<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.mime_type = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for mime_type: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<AudioContent> for super::AudioContent {
        type Error = super::error::ConversionError;
        fn try_from(
            value: AudioContent,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                annotations: value.annotations?,
                data: value.data?,
                meta: value.meta?,
                mime_type: value.mime_type?,
            })
        }
    }
    impl ::std::convert::From<super::AudioContent> for AudioContent {
        fn from(value: super::AudioContent) -> Self {
            Self {
                annotations: Ok(value.annotations),
                data: Ok(value.data),
                meta: Ok(value.meta),
                mime_type: Ok(value.mime_type),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct AuthMethodAgent {
        description: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
        id: ::std::result::Result<super::AuthMethodId, ::std::string::String>,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        name: ::std::result::Result<::std::string::String, ::std::string::String>,
    }
    impl ::std::default::Default for AuthMethodAgent {
        fn default() -> Self {
            Self {
                description: Ok(Default::default()),
                id: Err("no value supplied for id".to_string()),
                meta: Ok(Default::default()),
                name: Err("no value supplied for name".to_string()),
            }
        }
    }
    impl AuthMethodAgent {
        pub fn description<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.description = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for description: {e}"));
            self
        }
        pub fn id<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::AuthMethodId>,
            T::Error: ::std::fmt::Display,
        {
            self.id = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for id: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn name<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.name = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for name: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<AuthMethodAgent> for super::AuthMethodAgent {
        type Error = super::error::ConversionError;
        fn try_from(
            value: AuthMethodAgent,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                description: value.description?,
                id: value.id?,
                meta: value.meta?,
                name: value.name?,
            })
        }
    }
    impl ::std::convert::From<super::AuthMethodAgent> for AuthMethodAgent {
        fn from(value: super::AuthMethodAgent) -> Self {
            Self {
                description: Ok(value.description),
                id: Ok(value.id),
                meta: Ok(value.meta),
                name: Ok(value.name),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct AuthenticateRequest {
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        method_id: ::std::result::Result<super::AuthMethodId, ::std::string::String>,
    }
    impl ::std::default::Default for AuthenticateRequest {
        fn default() -> Self {
            Self {
                meta: Ok(Default::default()),
                method_id: Err("no value supplied for method_id".to_string()),
            }
        }
    }
    impl AuthenticateRequest {
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn method_id<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::AuthMethodId>,
            T::Error: ::std::fmt::Display,
        {
            self.method_id = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for method_id: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<AuthenticateRequest> for super::AuthenticateRequest {
        type Error = super::error::ConversionError;
        fn try_from(
            value: AuthenticateRequest,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                meta: value.meta?,
                method_id: value.method_id?,
            })
        }
    }
    impl ::std::convert::From<super::AuthenticateRequest> for AuthenticateRequest {
        fn from(value: super::AuthenticateRequest) -> Self {
            Self {
                meta: Ok(value.meta),
                method_id: Ok(value.method_id),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct AuthenticateResponse {
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for AuthenticateResponse {
        fn default() -> Self {
            Self {
                meta: Ok(Default::default()),
            }
        }
    }
    impl AuthenticateResponse {
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<AuthenticateResponse> for super::AuthenticateResponse {
        type Error = super::error::ConversionError;
        fn try_from(
            value: AuthenticateResponse,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self { meta: value.meta? })
        }
    }
    impl ::std::convert::From<super::AuthenticateResponse> for AuthenticateResponse {
        fn from(value: super::AuthenticateResponse) -> Self {
            Self {
                meta: Ok(value.meta),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct AvailableCommand {
        description: ::std::result::Result<::std::string::String, ::std::string::String>,
        input: ::std::result::Result<
            ::std::option::Option<super::AvailableCommandInput>,
            ::std::string::String,
        >,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        name: ::std::result::Result<::std::string::String, ::std::string::String>,
    }
    impl ::std::default::Default for AvailableCommand {
        fn default() -> Self {
            Self {
                description: Err("no value supplied for description".to_string()),
                input: Ok(Default::default()),
                meta: Ok(Default::default()),
                name: Err("no value supplied for name".to_string()),
            }
        }
    }
    impl AvailableCommand {
        pub fn description<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.description = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for description: {e}"));
            self
        }
        pub fn input<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::AvailableCommandInput>>,
            T::Error: ::std::fmt::Display,
        {
            self.input = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for input: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn name<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.name = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for name: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<AvailableCommand> for super::AvailableCommand {
        type Error = super::error::ConversionError;
        fn try_from(
            value: AvailableCommand,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                description: value.description?,
                input: value.input?,
                meta: value.meta?,
                name: value.name?,
            })
        }
    }
    impl ::std::convert::From<super::AvailableCommand> for AvailableCommand {
        fn from(value: super::AvailableCommand) -> Self {
            Self {
                description: Ok(value.description),
                input: Ok(value.input),
                meta: Ok(value.meta),
                name: Ok(value.name),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct AvailableCommandsUpdate {
        available_commands:
            ::std::result::Result<::std::vec::Vec<super::AvailableCommand>, ::std::string::String>,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for AvailableCommandsUpdate {
        fn default() -> Self {
            Self {
                available_commands: Err("no value supplied for available_commands".to_string()),
                meta: Ok(Default::default()),
            }
        }
    }
    impl AvailableCommandsUpdate {
        pub fn available_commands<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::vec::Vec<super::AvailableCommand>>,
            T::Error: ::std::fmt::Display,
        {
            self.available_commands = value.try_into().map_err(|e| {
                format!("error converting supplied value for available_commands: {e}")
            });
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<AvailableCommandsUpdate> for super::AvailableCommandsUpdate {
        type Error = super::error::ConversionError;
        fn try_from(
            value: AvailableCommandsUpdate,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                available_commands: value.available_commands?,
                meta: value.meta?,
            })
        }
    }
    impl ::std::convert::From<super::AvailableCommandsUpdate> for AvailableCommandsUpdate {
        fn from(value: super::AvailableCommandsUpdate) -> Self {
            Self {
                available_commands: Ok(value.available_commands),
                meta: Ok(value.meta),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct BlobResourceContents {
        blob: ::std::result::Result<::std::string::String, ::std::string::String>,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        mime_type: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
        uri: ::std::result::Result<::std::string::String, ::std::string::String>,
    }
    impl ::std::default::Default for BlobResourceContents {
        fn default() -> Self {
            Self {
                blob: Err("no value supplied for blob".to_string()),
                meta: Ok(Default::default()),
                mime_type: Ok(Default::default()),
                uri: Err("no value supplied for uri".to_string()),
            }
        }
    }
    impl BlobResourceContents {
        pub fn blob<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.blob = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for blob: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn mime_type<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.mime_type = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for mime_type: {e}"));
            self
        }
        pub fn uri<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.uri = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for uri: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<BlobResourceContents> for super::BlobResourceContents {
        type Error = super::error::ConversionError;
        fn try_from(
            value: BlobResourceContents,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                blob: value.blob?,
                meta: value.meta?,
                mime_type: value.mime_type?,
                uri: value.uri?,
            })
        }
    }
    impl ::std::convert::From<super::BlobResourceContents> for BlobResourceContents {
        fn from(value: super::BlobResourceContents) -> Self {
            Self {
                blob: Ok(value.blob),
                meta: Ok(value.meta),
                mime_type: Ok(value.mime_type),
                uri: Ok(value.uri),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct BooleanConfigOptionCapabilities {
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for BooleanConfigOptionCapabilities {
        fn default() -> Self {
            Self {
                meta: Ok(Default::default()),
            }
        }
    }
    impl BooleanConfigOptionCapabilities {
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<BooleanConfigOptionCapabilities>
        for super::BooleanConfigOptionCapabilities
    {
        type Error = super::error::ConversionError;
        fn try_from(
            value: BooleanConfigOptionCapabilities,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self { meta: value.meta? })
        }
    }
    impl ::std::convert::From<super::BooleanConfigOptionCapabilities>
        for BooleanConfigOptionCapabilities
    {
        fn from(value: super::BooleanConfigOptionCapabilities) -> Self {
            Self {
                meta: Ok(value.meta),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct CancelNotification {
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        session_id: ::std::result::Result<super::SessionId, ::std::string::String>,
    }
    impl ::std::default::Default for CancelNotification {
        fn default() -> Self {
            Self {
                meta: Ok(Default::default()),
                session_id: Err("no value supplied for session_id".to_string()),
            }
        }
    }
    impl CancelNotification {
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn session_id<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::SessionId>,
            T::Error: ::std::fmt::Display,
        {
            self.session_id = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for session_id: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<CancelNotification> for super::CancelNotification {
        type Error = super::error::ConversionError;
        fn try_from(
            value: CancelNotification,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                meta: value.meta?,
                session_id: value.session_id?,
            })
        }
    }
    impl ::std::convert::From<super::CancelNotification> for CancelNotification {
        fn from(value: super::CancelNotification) -> Self {
            Self {
                meta: Ok(value.meta),
                session_id: Ok(value.session_id),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct CancelRequestNotification {
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        request_id: ::std::result::Result<super::RequestId, ::std::string::String>,
    }
    impl ::std::default::Default for CancelRequestNotification {
        fn default() -> Self {
            Self {
                meta: Ok(Default::default()),
                request_id: Err("no value supplied for request_id".to_string()),
            }
        }
    }
    impl CancelRequestNotification {
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn request_id<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::RequestId>,
            T::Error: ::std::fmt::Display,
        {
            self.request_id = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for request_id: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<CancelRequestNotification> for super::CancelRequestNotification {
        type Error = super::error::ConversionError;
        fn try_from(
            value: CancelRequestNotification,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                meta: value.meta?,
                request_id: value.request_id?,
            })
        }
    }
    impl ::std::convert::From<super::CancelRequestNotification> for CancelRequestNotification {
        fn from(value: super::CancelRequestNotification) -> Self {
            Self {
                meta: Ok(value.meta),
                request_id: Ok(value.request_id),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct ClientCapabilities {
        fs: ::std::result::Result<super::FileSystemCapabilities, ::std::string::String>,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        session: ::std::result::Result<
            ::std::option::Option<super::ClientSessionCapabilities>,
            ::std::string::String,
        >,
        terminal: ::std::result::Result<bool, ::std::string::String>,
    }
    impl ::std::default::Default for ClientCapabilities {
        fn default() -> Self {
            Self {
                fs: Ok(super::defaults::client_capabilities_fs()),
                meta: Ok(Default::default()),
                session: Ok(Default::default()),
                terminal: Ok(Default::default()),
            }
        }
    }
    impl ClientCapabilities {
        pub fn fs<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::FileSystemCapabilities>,
            T::Error: ::std::fmt::Display,
        {
            self.fs = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for fs: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn session<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::ClientSessionCapabilities>>,
            T::Error: ::std::fmt::Display,
        {
            self.session = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for session: {e}"));
            self
        }
        pub fn terminal<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<bool>,
            T::Error: ::std::fmt::Display,
        {
            self.terminal = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for terminal: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<ClientCapabilities> for super::ClientCapabilities {
        type Error = super::error::ConversionError;
        fn try_from(
            value: ClientCapabilities,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                fs: value.fs?,
                meta: value.meta?,
                session: value.session?,
                terminal: value.terminal?,
            })
        }
    }
    impl ::std::convert::From<super::ClientCapabilities> for ClientCapabilities {
        fn from(value: super::ClientCapabilities) -> Self {
            Self {
                fs: Ok(value.fs),
                meta: Ok(value.meta),
                session: Ok(value.session),
                terminal: Ok(value.terminal),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct ClientNotification {
        method: ::std::result::Result<::std::string::String, ::std::string::String>,
        params: ::std::result::Result<
            ::std::option::Option<super::ClientNotificationParams>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for ClientNotification {
        fn default() -> Self {
            Self {
                method: Err("no value supplied for method".to_string()),
                params: Ok(Default::default()),
            }
        }
    }
    impl ClientNotification {
        pub fn method<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.method = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for method: {e}"));
            self
        }
        pub fn params<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::ClientNotificationParams>>,
            T::Error: ::std::fmt::Display,
        {
            self.params = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for params: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<ClientNotification> for super::ClientNotification {
        type Error = super::error::ConversionError;
        fn try_from(
            value: ClientNotification,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                method: value.method?,
                params: value.params?,
            })
        }
    }
    impl ::std::convert::From<super::ClientNotification> for ClientNotification {
        fn from(value: super::ClientNotification) -> Self {
            Self {
                method: Ok(value.method),
                params: Ok(value.params),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct ClientNotificationParams {
        subtype_0: ::std::result::Result<
            ::std::option::Option<super::CancelNotification>,
            ::std::string::String,
        >,
        subtype_1: ::std::result::Result<
            ::std::option::Option<super::ExtNotification>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for ClientNotificationParams {
        fn default() -> Self {
            Self {
                subtype_0: Ok(Default::default()),
                subtype_1: Ok(Default::default()),
            }
        }
    }
    impl ClientNotificationParams {
        pub fn subtype_0<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::CancelNotification>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_0 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_0: {e}"));
            self
        }
        pub fn subtype_1<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::ExtNotification>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_1 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_1: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<ClientNotificationParams> for super::ClientNotificationParams {
        type Error = super::error::ConversionError;
        fn try_from(
            value: ClientNotificationParams,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                subtype_0: value.subtype_0?,
                subtype_1: value.subtype_1?,
            })
        }
    }
    impl ::std::convert::From<super::ClientNotificationParams> for ClientNotificationParams {
        fn from(value: super::ClientNotificationParams) -> Self {
            Self {
                subtype_0: Ok(value.subtype_0),
                subtype_1: Ok(value.subtype_1),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct ClientRequest {
        id: ::std::result::Result<super::RequestId, ::std::string::String>,
        method: ::std::result::Result<::std::string::String, ::std::string::String>,
        params: ::std::result::Result<
            ::std::option::Option<super::ClientRequestParams>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for ClientRequest {
        fn default() -> Self {
            Self {
                id: Err("no value supplied for id".to_string()),
                method: Err("no value supplied for method".to_string()),
                params: Ok(Default::default()),
            }
        }
    }
    impl ClientRequest {
        pub fn id<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::RequestId>,
            T::Error: ::std::fmt::Display,
        {
            self.id = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for id: {e}"));
            self
        }
        pub fn method<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.method = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for method: {e}"));
            self
        }
        pub fn params<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::ClientRequestParams>>,
            T::Error: ::std::fmt::Display,
        {
            self.params = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for params: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<ClientRequest> for super::ClientRequest {
        type Error = super::error::ConversionError;
        fn try_from(
            value: ClientRequest,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                id: value.id?,
                method: value.method?,
                params: value.params?,
            })
        }
    }
    impl ::std::convert::From<super::ClientRequest> for ClientRequest {
        fn from(value: super::ClientRequest) -> Self {
            Self {
                id: Ok(value.id),
                method: Ok(value.method),
                params: Ok(value.params),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct ClientRequestParams {
        subtype_0: ::std::result::Result<
            ::std::option::Option<super::InitializeRequest>,
            ::std::string::String,
        >,
        subtype_1: ::std::result::Result<
            ::std::option::Option<super::AuthenticateRequest>,
            ::std::string::String,
        >,
        subtype_2: ::std::result::Result<
            ::std::option::Option<super::LogoutRequest>,
            ::std::string::String,
        >,
        subtype_3: ::std::result::Result<
            ::std::option::Option<super::NewSessionRequest>,
            ::std::string::String,
        >,
        subtype_4: ::std::result::Result<
            ::std::option::Option<super::LoadSessionRequest>,
            ::std::string::String,
        >,
        subtype_5: ::std::result::Result<
            ::std::option::Option<super::ListSessionsRequest>,
            ::std::string::String,
        >,
        subtype_6: ::std::result::Result<
            ::std::option::Option<super::DeleteSessionRequest>,
            ::std::string::String,
        >,
        subtype_7: ::std::result::Result<
            ::std::option::Option<super::ResumeSessionRequest>,
            ::std::string::String,
        >,
        subtype_8: ::std::result::Result<
            ::std::option::Option<super::CloseSessionRequest>,
            ::std::string::String,
        >,
        subtype_9: ::std::result::Result<
            ::std::option::Option<super::SetSessionModeRequest>,
            ::std::string::String,
        >,
        subtype_10: ::std::result::Result<
            ::std::option::Option<super::SetSessionConfigOptionRequest>,
            ::std::string::String,
        >,
        subtype_11: ::std::result::Result<
            ::std::option::Option<super::PromptRequest>,
            ::std::string::String,
        >,
        subtype_12:
            ::std::result::Result<::std::option::Option<super::ExtRequest>, ::std::string::String>,
    }
    impl ::std::default::Default for ClientRequestParams {
        fn default() -> Self {
            Self {
                subtype_0: Ok(Default::default()),
                subtype_1: Ok(Default::default()),
                subtype_2: Ok(Default::default()),
                subtype_3: Ok(Default::default()),
                subtype_4: Ok(Default::default()),
                subtype_5: Ok(Default::default()),
                subtype_6: Ok(Default::default()),
                subtype_7: Ok(Default::default()),
                subtype_8: Ok(Default::default()),
                subtype_9: Ok(Default::default()),
                subtype_10: Ok(Default::default()),
                subtype_11: Ok(Default::default()),
                subtype_12: Ok(Default::default()),
            }
        }
    }
    impl ClientRequestParams {
        pub fn subtype_0<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::InitializeRequest>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_0 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_0: {e}"));
            self
        }
        pub fn subtype_1<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::AuthenticateRequest>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_1 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_1: {e}"));
            self
        }
        pub fn subtype_2<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::LogoutRequest>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_2 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_2: {e}"));
            self
        }
        pub fn subtype_3<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::NewSessionRequest>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_3 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_3: {e}"));
            self
        }
        pub fn subtype_4<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::LoadSessionRequest>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_4 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_4: {e}"));
            self
        }
        pub fn subtype_5<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::ListSessionsRequest>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_5 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_5: {e}"));
            self
        }
        pub fn subtype_6<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::DeleteSessionRequest>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_6 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_6: {e}"));
            self
        }
        pub fn subtype_7<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::ResumeSessionRequest>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_7 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_7: {e}"));
            self
        }
        pub fn subtype_8<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::CloseSessionRequest>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_8 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_8: {e}"));
            self
        }
        pub fn subtype_9<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::SetSessionModeRequest>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_9 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_9: {e}"));
            self
        }
        pub fn subtype_10<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::SetSessionConfigOptionRequest>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_10 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_10: {e}"));
            self
        }
        pub fn subtype_11<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::PromptRequest>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_11 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_11: {e}"));
            self
        }
        pub fn subtype_12<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::ExtRequest>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_12 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_12: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<ClientRequestParams> for super::ClientRequestParams {
        type Error = super::error::ConversionError;
        fn try_from(
            value: ClientRequestParams,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                subtype_0: value.subtype_0?,
                subtype_1: value.subtype_1?,
                subtype_2: value.subtype_2?,
                subtype_3: value.subtype_3?,
                subtype_4: value.subtype_4?,
                subtype_5: value.subtype_5?,
                subtype_6: value.subtype_6?,
                subtype_7: value.subtype_7?,
                subtype_8: value.subtype_8?,
                subtype_9: value.subtype_9?,
                subtype_10: value.subtype_10?,
                subtype_11: value.subtype_11?,
                subtype_12: value.subtype_12?,
            })
        }
    }
    impl ::std::convert::From<super::ClientRequestParams> for ClientRequestParams {
        fn from(value: super::ClientRequestParams) -> Self {
            Self {
                subtype_0: Ok(value.subtype_0),
                subtype_1: Ok(value.subtype_1),
                subtype_2: Ok(value.subtype_2),
                subtype_3: Ok(value.subtype_3),
                subtype_4: Ok(value.subtype_4),
                subtype_5: Ok(value.subtype_5),
                subtype_6: Ok(value.subtype_6),
                subtype_7: Ok(value.subtype_7),
                subtype_8: Ok(value.subtype_8),
                subtype_9: Ok(value.subtype_9),
                subtype_10: Ok(value.subtype_10),
                subtype_11: Ok(value.subtype_11),
                subtype_12: Ok(value.subtype_12),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct ClientSessionCapabilities {
        config_options: ::std::result::Result<
            ::std::option::Option<super::SessionConfigOptionsCapabilities>,
            ::std::string::String,
        >,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for ClientSessionCapabilities {
        fn default() -> Self {
            Self {
                config_options: Ok(Default::default()),
                meta: Ok(Default::default()),
            }
        }
    }
    impl ClientSessionCapabilities {
        pub fn config_options<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<super::SessionConfigOptionsCapabilities>,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.config_options = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for config_options: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<ClientSessionCapabilities> for super::ClientSessionCapabilities {
        type Error = super::error::ConversionError;
        fn try_from(
            value: ClientSessionCapabilities,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                config_options: value.config_options?,
                meta: value.meta?,
            })
        }
    }
    impl ::std::convert::From<super::ClientSessionCapabilities> for ClientSessionCapabilities {
        fn from(value: super::ClientSessionCapabilities) -> Self {
            Self {
                config_options: Ok(value.config_options),
                meta: Ok(value.meta),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct ClientVariant0Params {
        subtype_0: ::std::result::Result<
            ::std::option::Option<super::InitializeRequest>,
            ::std::string::String,
        >,
        subtype_1: ::std::result::Result<
            ::std::option::Option<super::AuthenticateRequest>,
            ::std::string::String,
        >,
        subtype_2: ::std::result::Result<
            ::std::option::Option<super::LogoutRequest>,
            ::std::string::String,
        >,
        subtype_3: ::std::result::Result<
            ::std::option::Option<super::NewSessionRequest>,
            ::std::string::String,
        >,
        subtype_4: ::std::result::Result<
            ::std::option::Option<super::LoadSessionRequest>,
            ::std::string::String,
        >,
        subtype_5: ::std::result::Result<
            ::std::option::Option<super::ListSessionsRequest>,
            ::std::string::String,
        >,
        subtype_6: ::std::result::Result<
            ::std::option::Option<super::DeleteSessionRequest>,
            ::std::string::String,
        >,
        subtype_7: ::std::result::Result<
            ::std::option::Option<super::ResumeSessionRequest>,
            ::std::string::String,
        >,
        subtype_8: ::std::result::Result<
            ::std::option::Option<super::CloseSessionRequest>,
            ::std::string::String,
        >,
        subtype_9: ::std::result::Result<
            ::std::option::Option<super::SetSessionModeRequest>,
            ::std::string::String,
        >,
        subtype_10: ::std::result::Result<
            ::std::option::Option<super::SetSessionConfigOptionRequest>,
            ::std::string::String,
        >,
        subtype_11: ::std::result::Result<
            ::std::option::Option<super::PromptRequest>,
            ::std::string::String,
        >,
        subtype_12:
            ::std::result::Result<::std::option::Option<super::ExtRequest>, ::std::string::String>,
    }
    impl ::std::default::Default for ClientVariant0Params {
        fn default() -> Self {
            Self {
                subtype_0: Ok(Default::default()),
                subtype_1: Ok(Default::default()),
                subtype_2: Ok(Default::default()),
                subtype_3: Ok(Default::default()),
                subtype_4: Ok(Default::default()),
                subtype_5: Ok(Default::default()),
                subtype_6: Ok(Default::default()),
                subtype_7: Ok(Default::default()),
                subtype_8: Ok(Default::default()),
                subtype_9: Ok(Default::default()),
                subtype_10: Ok(Default::default()),
                subtype_11: Ok(Default::default()),
                subtype_12: Ok(Default::default()),
            }
        }
    }
    impl ClientVariant0Params {
        pub fn subtype_0<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::InitializeRequest>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_0 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_0: {e}"));
            self
        }
        pub fn subtype_1<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::AuthenticateRequest>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_1 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_1: {e}"));
            self
        }
        pub fn subtype_2<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::LogoutRequest>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_2 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_2: {e}"));
            self
        }
        pub fn subtype_3<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::NewSessionRequest>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_3 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_3: {e}"));
            self
        }
        pub fn subtype_4<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::LoadSessionRequest>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_4 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_4: {e}"));
            self
        }
        pub fn subtype_5<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::ListSessionsRequest>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_5 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_5: {e}"));
            self
        }
        pub fn subtype_6<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::DeleteSessionRequest>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_6 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_6: {e}"));
            self
        }
        pub fn subtype_7<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::ResumeSessionRequest>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_7 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_7: {e}"));
            self
        }
        pub fn subtype_8<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::CloseSessionRequest>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_8 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_8: {e}"));
            self
        }
        pub fn subtype_9<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::SetSessionModeRequest>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_9 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_9: {e}"));
            self
        }
        pub fn subtype_10<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::SetSessionConfigOptionRequest>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_10 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_10: {e}"));
            self
        }
        pub fn subtype_11<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::PromptRequest>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_11 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_11: {e}"));
            self
        }
        pub fn subtype_12<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::ExtRequest>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_12 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_12: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<ClientVariant0Params> for super::ClientVariant0Params {
        type Error = super::error::ConversionError;
        fn try_from(
            value: ClientVariant0Params,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                subtype_0: value.subtype_0?,
                subtype_1: value.subtype_1?,
                subtype_2: value.subtype_2?,
                subtype_3: value.subtype_3?,
                subtype_4: value.subtype_4?,
                subtype_5: value.subtype_5?,
                subtype_6: value.subtype_6?,
                subtype_7: value.subtype_7?,
                subtype_8: value.subtype_8?,
                subtype_9: value.subtype_9?,
                subtype_10: value.subtype_10?,
                subtype_11: value.subtype_11?,
                subtype_12: value.subtype_12?,
            })
        }
    }
    impl ::std::convert::From<super::ClientVariant0Params> for ClientVariant0Params {
        fn from(value: super::ClientVariant0Params) -> Self {
            Self {
                subtype_0: Ok(value.subtype_0),
                subtype_1: Ok(value.subtype_1),
                subtype_2: Ok(value.subtype_2),
                subtype_3: Ok(value.subtype_3),
                subtype_4: Ok(value.subtype_4),
                subtype_5: Ok(value.subtype_5),
                subtype_6: Ok(value.subtype_6),
                subtype_7: Ok(value.subtype_7),
                subtype_8: Ok(value.subtype_8),
                subtype_9: Ok(value.subtype_9),
                subtype_10: Ok(value.subtype_10),
                subtype_11: Ok(value.subtype_11),
                subtype_12: Ok(value.subtype_12),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct ClientVariant2Params {
        subtype_0: ::std::result::Result<
            ::std::option::Option<super::CancelNotification>,
            ::std::string::String,
        >,
        subtype_1: ::std::result::Result<
            ::std::option::Option<super::ExtNotification>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for ClientVariant2Params {
        fn default() -> Self {
            Self {
                subtype_0: Ok(Default::default()),
                subtype_1: Ok(Default::default()),
            }
        }
    }
    impl ClientVariant2Params {
        pub fn subtype_0<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::CancelNotification>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_0 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_0: {e}"));
            self
        }
        pub fn subtype_1<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::ExtNotification>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_1 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_1: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<ClientVariant2Params> for super::ClientVariant2Params {
        type Error = super::error::ConversionError;
        fn try_from(
            value: ClientVariant2Params,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                subtype_0: value.subtype_0?,
                subtype_1: value.subtype_1?,
            })
        }
    }
    impl ::std::convert::From<super::ClientVariant2Params> for ClientVariant2Params {
        fn from(value: super::ClientVariant2Params) -> Self {
            Self {
                subtype_0: Ok(value.subtype_0),
                subtype_1: Ok(value.subtype_1),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct CloseSessionRequest {
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        session_id: ::std::result::Result<super::SessionId, ::std::string::String>,
    }
    impl ::std::default::Default for CloseSessionRequest {
        fn default() -> Self {
            Self {
                meta: Ok(Default::default()),
                session_id: Err("no value supplied for session_id".to_string()),
            }
        }
    }
    impl CloseSessionRequest {
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn session_id<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::SessionId>,
            T::Error: ::std::fmt::Display,
        {
            self.session_id = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for session_id: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<CloseSessionRequest> for super::CloseSessionRequest {
        type Error = super::error::ConversionError;
        fn try_from(
            value: CloseSessionRequest,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                meta: value.meta?,
                session_id: value.session_id?,
            })
        }
    }
    impl ::std::convert::From<super::CloseSessionRequest> for CloseSessionRequest {
        fn from(value: super::CloseSessionRequest) -> Self {
            Self {
                meta: Ok(value.meta),
                session_id: Ok(value.session_id),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct CloseSessionResponse {
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for CloseSessionResponse {
        fn default() -> Self {
            Self {
                meta: Ok(Default::default()),
            }
        }
    }
    impl CloseSessionResponse {
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<CloseSessionResponse> for super::CloseSessionResponse {
        type Error = super::error::ConversionError;
        fn try_from(
            value: CloseSessionResponse,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self { meta: value.meta? })
        }
    }
    impl ::std::convert::From<super::CloseSessionResponse> for CloseSessionResponse {
        fn from(value: super::CloseSessionResponse) -> Self {
            Self {
                meta: Ok(value.meta),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct ConfigOptionUpdate {
        config_options: ::std::result::Result<
            ::std::vec::Vec<super::SessionConfigOption>,
            ::std::string::String,
        >,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for ConfigOptionUpdate {
        fn default() -> Self {
            Self {
                config_options: Err("no value supplied for config_options".to_string()),
                meta: Ok(Default::default()),
            }
        }
    }
    impl ConfigOptionUpdate {
        pub fn config_options<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::vec::Vec<super::SessionConfigOption>>,
            T::Error: ::std::fmt::Display,
        {
            self.config_options = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for config_options: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<ConfigOptionUpdate> for super::ConfigOptionUpdate {
        type Error = super::error::ConversionError;
        fn try_from(
            value: ConfigOptionUpdate,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                config_options: value.config_options?,
                meta: value.meta?,
            })
        }
    }
    impl ::std::convert::From<super::ConfigOptionUpdate> for ConfigOptionUpdate {
        fn from(value: super::ConfigOptionUpdate) -> Self {
            Self {
                config_options: Ok(value.config_options),
                meta: Ok(value.meta),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct Content {
        content: ::std::result::Result<super::ContentBlock, ::std::string::String>,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for Content {
        fn default() -> Self {
            Self {
                content: Err("no value supplied for content".to_string()),
                meta: Ok(Default::default()),
            }
        }
    }
    impl Content {
        pub fn content<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::ContentBlock>,
            T::Error: ::std::fmt::Display,
        {
            self.content = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for content: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<Content> for super::Content {
        type Error = super::error::ConversionError;
        fn try_from(value: Content) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                content: value.content?,
                meta: value.meta?,
            })
        }
    }
    impl ::std::convert::From<super::Content> for Content {
        fn from(value: super::Content) -> Self {
            Self {
                content: Ok(value.content),
                meta: Ok(value.meta),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct ContentChunk {
        content: ::std::result::Result<super::ContentBlock, ::std::string::String>,
        message_id:
            ::std::result::Result<::std::option::Option<super::MessageId>, ::std::string::String>,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for ContentChunk {
        fn default() -> Self {
            Self {
                content: Err("no value supplied for content".to_string()),
                message_id: Ok(Default::default()),
                meta: Ok(Default::default()),
            }
        }
    }
    impl ContentChunk {
        pub fn content<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::ContentBlock>,
            T::Error: ::std::fmt::Display,
        {
            self.content = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for content: {e}"));
            self
        }
        pub fn message_id<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::MessageId>>,
            T::Error: ::std::fmt::Display,
        {
            self.message_id = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for message_id: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<ContentChunk> for super::ContentChunk {
        type Error = super::error::ConversionError;
        fn try_from(
            value: ContentChunk,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                content: value.content?,
                message_id: value.message_id?,
                meta: value.meta?,
            })
        }
    }
    impl ::std::convert::From<super::ContentChunk> for ContentChunk {
        fn from(value: super::ContentChunk) -> Self {
            Self {
                content: Ok(value.content),
                message_id: Ok(value.message_id),
                meta: Ok(value.meta),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct Cost {
        amount: ::std::result::Result<f64, ::std::string::String>,
        currency: ::std::result::Result<::std::string::String, ::std::string::String>,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for Cost {
        fn default() -> Self {
            Self {
                amount: Err("no value supplied for amount".to_string()),
                currency: Err("no value supplied for currency".to_string()),
                meta: Ok(Default::default()),
            }
        }
    }
    impl Cost {
        pub fn amount<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<f64>,
            T::Error: ::std::fmt::Display,
        {
            self.amount = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for amount: {e}"));
            self
        }
        pub fn currency<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.currency = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for currency: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<Cost> for super::Cost {
        type Error = super::error::ConversionError;
        fn try_from(value: Cost) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                amount: value.amount?,
                currency: value.currency?,
                meta: value.meta?,
            })
        }
    }
    impl ::std::convert::From<super::Cost> for Cost {
        fn from(value: super::Cost) -> Self {
            Self {
                amount: Ok(value.amount),
                currency: Ok(value.currency),
                meta: Ok(value.meta),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct CreateTerminalRequest {
        args: ::std::result::Result<::std::vec::Vec<::std::string::String>, ::std::string::String>,
        command: ::std::result::Result<::std::string::String, ::std::string::String>,
        cwd: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
        env: ::std::result::Result<::std::vec::Vec<super::EnvVariable>, ::std::string::String>,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        output_byte_limit: ::std::result::Result<::std::option::Option<u64>, ::std::string::String>,
        session_id: ::std::result::Result<super::SessionId, ::std::string::String>,
    }
    impl ::std::default::Default for CreateTerminalRequest {
        fn default() -> Self {
            Self {
                args: Ok(Default::default()),
                command: Err("no value supplied for command".to_string()),
                cwd: Ok(Default::default()),
                env: Ok(Default::default()),
                meta: Ok(Default::default()),
                output_byte_limit: Ok(Default::default()),
                session_id: Err("no value supplied for session_id".to_string()),
            }
        }
    }
    impl CreateTerminalRequest {
        pub fn args<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::vec::Vec<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.args = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for args: {e}"));
            self
        }
        pub fn command<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.command = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for command: {e}"));
            self
        }
        pub fn cwd<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.cwd = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for cwd: {e}"));
            self
        }
        pub fn env<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::vec::Vec<super::EnvVariable>>,
            T::Error: ::std::fmt::Display,
        {
            self.env = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for env: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn output_byte_limit<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<u64>>,
            T::Error: ::std::fmt::Display,
        {
            self.output_byte_limit = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for output_byte_limit: {e}"));
            self
        }
        pub fn session_id<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::SessionId>,
            T::Error: ::std::fmt::Display,
        {
            self.session_id = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for session_id: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<CreateTerminalRequest> for super::CreateTerminalRequest {
        type Error = super::error::ConversionError;
        fn try_from(
            value: CreateTerminalRequest,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                args: value.args?,
                command: value.command?,
                cwd: value.cwd?,
                env: value.env?,
                meta: value.meta?,
                output_byte_limit: value.output_byte_limit?,
                session_id: value.session_id?,
            })
        }
    }
    impl ::std::convert::From<super::CreateTerminalRequest> for CreateTerminalRequest {
        fn from(value: super::CreateTerminalRequest) -> Self {
            Self {
                args: Ok(value.args),
                command: Ok(value.command),
                cwd: Ok(value.cwd),
                env: Ok(value.env),
                meta: Ok(value.meta),
                output_byte_limit: Ok(value.output_byte_limit),
                session_id: Ok(value.session_id),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct CreateTerminalResponse {
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        terminal_id: ::std::result::Result<super::TerminalId, ::std::string::String>,
    }
    impl ::std::default::Default for CreateTerminalResponse {
        fn default() -> Self {
            Self {
                meta: Ok(Default::default()),
                terminal_id: Err("no value supplied for terminal_id".to_string()),
            }
        }
    }
    impl CreateTerminalResponse {
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn terminal_id<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::TerminalId>,
            T::Error: ::std::fmt::Display,
        {
            self.terminal_id = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for terminal_id: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<CreateTerminalResponse> for super::CreateTerminalResponse {
        type Error = super::error::ConversionError;
        fn try_from(
            value: CreateTerminalResponse,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                meta: value.meta?,
                terminal_id: value.terminal_id?,
            })
        }
    }
    impl ::std::convert::From<super::CreateTerminalResponse> for CreateTerminalResponse {
        fn from(value: super::CreateTerminalResponse) -> Self {
            Self {
                meta: Ok(value.meta),
                terminal_id: Ok(value.terminal_id),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct CurrentModeUpdate {
        current_mode_id: ::std::result::Result<super::SessionModeId, ::std::string::String>,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for CurrentModeUpdate {
        fn default() -> Self {
            Self {
                current_mode_id: Err("no value supplied for current_mode_id".to_string()),
                meta: Ok(Default::default()),
            }
        }
    }
    impl CurrentModeUpdate {
        pub fn current_mode_id<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::SessionModeId>,
            T::Error: ::std::fmt::Display,
        {
            self.current_mode_id = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for current_mode_id: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<CurrentModeUpdate> for super::CurrentModeUpdate {
        type Error = super::error::ConversionError;
        fn try_from(
            value: CurrentModeUpdate,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                current_mode_id: value.current_mode_id?,
                meta: value.meta?,
            })
        }
    }
    impl ::std::convert::From<super::CurrentModeUpdate> for CurrentModeUpdate {
        fn from(value: super::CurrentModeUpdate) -> Self {
            Self {
                current_mode_id: Ok(value.current_mode_id),
                meta: Ok(value.meta),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct DeleteSessionRequest {
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        session_id: ::std::result::Result<super::SessionId, ::std::string::String>,
    }
    impl ::std::default::Default for DeleteSessionRequest {
        fn default() -> Self {
            Self {
                meta: Ok(Default::default()),
                session_id: Err("no value supplied for session_id".to_string()),
            }
        }
    }
    impl DeleteSessionRequest {
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn session_id<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::SessionId>,
            T::Error: ::std::fmt::Display,
        {
            self.session_id = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for session_id: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<DeleteSessionRequest> for super::DeleteSessionRequest {
        type Error = super::error::ConversionError;
        fn try_from(
            value: DeleteSessionRequest,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                meta: value.meta?,
                session_id: value.session_id?,
            })
        }
    }
    impl ::std::convert::From<super::DeleteSessionRequest> for DeleteSessionRequest {
        fn from(value: super::DeleteSessionRequest) -> Self {
            Self {
                meta: Ok(value.meta),
                session_id: Ok(value.session_id),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct DeleteSessionResponse {
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for DeleteSessionResponse {
        fn default() -> Self {
            Self {
                meta: Ok(Default::default()),
            }
        }
    }
    impl DeleteSessionResponse {
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<DeleteSessionResponse> for super::DeleteSessionResponse {
        type Error = super::error::ConversionError;
        fn try_from(
            value: DeleteSessionResponse,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self { meta: value.meta? })
        }
    }
    impl ::std::convert::From<super::DeleteSessionResponse> for DeleteSessionResponse {
        fn from(value: super::DeleteSessionResponse) -> Self {
            Self {
                meta: Ok(value.meta),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct Diff {
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        new_text: ::std::result::Result<::std::string::String, ::std::string::String>,
        old_text: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
        path: ::std::result::Result<::std::string::String, ::std::string::String>,
    }
    impl ::std::default::Default for Diff {
        fn default() -> Self {
            Self {
                meta: Ok(Default::default()),
                new_text: Err("no value supplied for new_text".to_string()),
                old_text: Ok(Default::default()),
                path: Err("no value supplied for path".to_string()),
            }
        }
    }
    impl Diff {
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn new_text<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.new_text = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for new_text: {e}"));
            self
        }
        pub fn old_text<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.old_text = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for old_text: {e}"));
            self
        }
        pub fn path<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.path = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for path: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<Diff> for super::Diff {
        type Error = super::error::ConversionError;
        fn try_from(value: Diff) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                meta: value.meta?,
                new_text: value.new_text?,
                old_text: value.old_text?,
                path: value.path?,
            })
        }
    }
    impl ::std::convert::From<super::Diff> for Diff {
        fn from(value: super::Diff) -> Self {
            Self {
                meta: Ok(value.meta),
                new_text: Ok(value.new_text),
                old_text: Ok(value.old_text),
                path: Ok(value.path),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct EmbeddedResource {
        annotations:
            ::std::result::Result<::std::option::Option<super::Annotations>, ::std::string::String>,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        resource: ::std::result::Result<super::EmbeddedResourceResource, ::std::string::String>,
    }
    impl ::std::default::Default for EmbeddedResource {
        fn default() -> Self {
            Self {
                annotations: Ok(Default::default()),
                meta: Ok(Default::default()),
                resource: Err("no value supplied for resource".to_string()),
            }
        }
    }
    impl EmbeddedResource {
        pub fn annotations<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::Annotations>>,
            T::Error: ::std::fmt::Display,
        {
            self.annotations = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for annotations: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn resource<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::EmbeddedResourceResource>,
            T::Error: ::std::fmt::Display,
        {
            self.resource = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for resource: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<EmbeddedResource> for super::EmbeddedResource {
        type Error = super::error::ConversionError;
        fn try_from(
            value: EmbeddedResource,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                annotations: value.annotations?,
                meta: value.meta?,
                resource: value.resource?,
            })
        }
    }
    impl ::std::convert::From<super::EmbeddedResource> for EmbeddedResource {
        fn from(value: super::EmbeddedResource) -> Self {
            Self {
                annotations: Ok(value.annotations),
                meta: Ok(value.meta),
                resource: Ok(value.resource),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct EmbeddedResourceResource {
        subtype_0: ::std::result::Result<
            ::std::option::Option<super::TextResourceContents>,
            ::std::string::String,
        >,
        subtype_1: ::std::result::Result<
            ::std::option::Option<super::BlobResourceContents>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for EmbeddedResourceResource {
        fn default() -> Self {
            Self {
                subtype_0: Ok(Default::default()),
                subtype_1: Ok(Default::default()),
            }
        }
    }
    impl EmbeddedResourceResource {
        pub fn subtype_0<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::TextResourceContents>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_0 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_0: {e}"));
            self
        }
        pub fn subtype_1<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::BlobResourceContents>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_1 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_1: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<EmbeddedResourceResource> for super::EmbeddedResourceResource {
        type Error = super::error::ConversionError;
        fn try_from(
            value: EmbeddedResourceResource,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                subtype_0: value.subtype_0?,
                subtype_1: value.subtype_1?,
            })
        }
    }
    impl ::std::convert::From<super::EmbeddedResourceResource> for EmbeddedResourceResource {
        fn from(value: super::EmbeddedResourceResource) -> Self {
            Self {
                subtype_0: Ok(value.subtype_0),
                subtype_1: Ok(value.subtype_1),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct EnvVariable {
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        name: ::std::result::Result<::std::string::String, ::std::string::String>,
        value: ::std::result::Result<::std::string::String, ::std::string::String>,
    }
    impl ::std::default::Default for EnvVariable {
        fn default() -> Self {
            Self {
                meta: Ok(Default::default()),
                name: Err("no value supplied for name".to_string()),
                value: Err("no value supplied for value".to_string()),
            }
        }
    }
    impl EnvVariable {
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn name<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.name = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for name: {e}"));
            self
        }
        pub fn value<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.value = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for value: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<EnvVariable> for super::EnvVariable {
        type Error = super::error::ConversionError;
        fn try_from(
            value: EnvVariable,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                meta: value.meta?,
                name: value.name?,
                value: value.value?,
            })
        }
    }
    impl ::std::convert::From<super::EnvVariable> for EnvVariable {
        fn from(value: super::EnvVariable) -> Self {
            Self {
                meta: Ok(value.meta),
                name: Ok(value.name),
                value: Ok(value.value),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct Error {
        code: ::std::result::Result<super::ErrorCode, ::std::string::String>,
        data: ::std::result::Result<
            ::std::option::Option<::serde_json::Value>,
            ::std::string::String,
        >,
        message: ::std::result::Result<::std::string::String, ::std::string::String>,
    }
    impl ::std::default::Default for Error {
        fn default() -> Self {
            Self {
                code: Err("no value supplied for code".to_string()),
                data: Ok(Default::default()),
                message: Err("no value supplied for message".to_string()),
            }
        }
    }
    impl Error {
        pub fn code<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::ErrorCode>,
            T::Error: ::std::fmt::Display,
        {
            self.code = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for code: {e}"));
            self
        }
        pub fn data<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::serde_json::Value>>,
            T::Error: ::std::fmt::Display,
        {
            self.data = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for data: {e}"));
            self
        }
        pub fn message<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.message = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for message: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<Error> for super::Error {
        type Error = super::error::ConversionError;
        fn try_from(value: Error) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                code: value.code?,
                data: value.data?,
                message: value.message?,
            })
        }
    }
    impl ::std::convert::From<super::Error> for Error {
        fn from(value: super::Error) -> Self {
            Self {
                code: Ok(value.code),
                data: Ok(value.data),
                message: Ok(value.message),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct ErrorCode {
        subtype_0: ::std::result::Result<::std::option::Option<i32>, ::std::string::String>,
        subtype_1: ::std::result::Result<::std::option::Option<i32>, ::std::string::String>,
        subtype_2: ::std::result::Result<::std::option::Option<i32>, ::std::string::String>,
        subtype_3: ::std::result::Result<::std::option::Option<i32>, ::std::string::String>,
        subtype_4: ::std::result::Result<::std::option::Option<i32>, ::std::string::String>,
        subtype_5: ::std::result::Result<::std::option::Option<i32>, ::std::string::String>,
        subtype_6: ::std::result::Result<::std::option::Option<i32>, ::std::string::String>,
        subtype_7: ::std::result::Result<::std::option::Option<i32>, ::std::string::String>,
        subtype_8: ::std::result::Result<::std::option::Option<i32>, ::std::string::String>,
    }
    impl ::std::default::Default for ErrorCode {
        fn default() -> Self {
            Self {
                subtype_0: Ok(Default::default()),
                subtype_1: Ok(Default::default()),
                subtype_2: Ok(Default::default()),
                subtype_3: Ok(Default::default()),
                subtype_4: Ok(Default::default()),
                subtype_5: Ok(Default::default()),
                subtype_6: Ok(Default::default()),
                subtype_7: Ok(Default::default()),
                subtype_8: Ok(Default::default()),
            }
        }
    }
    impl ErrorCode {
        pub fn subtype_0<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<i32>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_0 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_0: {e}"));
            self
        }
        pub fn subtype_1<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<i32>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_1 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_1: {e}"));
            self
        }
        pub fn subtype_2<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<i32>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_2 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_2: {e}"));
            self
        }
        pub fn subtype_3<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<i32>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_3 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_3: {e}"));
            self
        }
        pub fn subtype_4<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<i32>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_4 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_4: {e}"));
            self
        }
        pub fn subtype_5<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<i32>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_5 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_5: {e}"));
            self
        }
        pub fn subtype_6<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<i32>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_6 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_6: {e}"));
            self
        }
        pub fn subtype_7<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<i32>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_7 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_7: {e}"));
            self
        }
        pub fn subtype_8<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<i32>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_8 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_8: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<ErrorCode> for super::ErrorCode {
        type Error = super::error::ConversionError;
        fn try_from(
            value: ErrorCode,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                subtype_0: value.subtype_0?,
                subtype_1: value.subtype_1?,
                subtype_2: value.subtype_2?,
                subtype_3: value.subtype_3?,
                subtype_4: value.subtype_4?,
                subtype_5: value.subtype_5?,
                subtype_6: value.subtype_6?,
                subtype_7: value.subtype_7?,
                subtype_8: value.subtype_8?,
            })
        }
    }
    impl ::std::convert::From<super::ErrorCode> for ErrorCode {
        fn from(value: super::ErrorCode) -> Self {
            Self {
                subtype_0: Ok(value.subtype_0),
                subtype_1: Ok(value.subtype_1),
                subtype_2: Ok(value.subtype_2),
                subtype_3: Ok(value.subtype_3),
                subtype_4: Ok(value.subtype_4),
                subtype_5: Ok(value.subtype_5),
                subtype_6: Ok(value.subtype_6),
                subtype_7: Ok(value.subtype_7),
                subtype_8: Ok(value.subtype_8),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct FileSystemCapabilities {
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        read_text_file: ::std::result::Result<bool, ::std::string::String>,
        write_text_file: ::std::result::Result<bool, ::std::string::String>,
    }
    impl ::std::default::Default for FileSystemCapabilities {
        fn default() -> Self {
            Self {
                meta: Ok(Default::default()),
                read_text_file: Ok(Default::default()),
                write_text_file: Ok(Default::default()),
            }
        }
    }
    impl FileSystemCapabilities {
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn read_text_file<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<bool>,
            T::Error: ::std::fmt::Display,
        {
            self.read_text_file = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for read_text_file: {e}"));
            self
        }
        pub fn write_text_file<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<bool>,
            T::Error: ::std::fmt::Display,
        {
            self.write_text_file = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for write_text_file: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<FileSystemCapabilities> for super::FileSystemCapabilities {
        type Error = super::error::ConversionError;
        fn try_from(
            value: FileSystemCapabilities,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                meta: value.meta?,
                read_text_file: value.read_text_file?,
                write_text_file: value.write_text_file?,
            })
        }
    }
    impl ::std::convert::From<super::FileSystemCapabilities> for FileSystemCapabilities {
        fn from(value: super::FileSystemCapabilities) -> Self {
            Self {
                meta: Ok(value.meta),
                read_text_file: Ok(value.read_text_file),
                write_text_file: Ok(value.write_text_file),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct HttpHeader {
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        name: ::std::result::Result<::std::string::String, ::std::string::String>,
        value: ::std::result::Result<::std::string::String, ::std::string::String>,
    }
    impl ::std::default::Default for HttpHeader {
        fn default() -> Self {
            Self {
                meta: Ok(Default::default()),
                name: Err("no value supplied for name".to_string()),
                value: Err("no value supplied for value".to_string()),
            }
        }
    }
    impl HttpHeader {
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn name<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.name = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for name: {e}"));
            self
        }
        pub fn value<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.value = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for value: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<HttpHeader> for super::HttpHeader {
        type Error = super::error::ConversionError;
        fn try_from(
            value: HttpHeader,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                meta: value.meta?,
                name: value.name?,
                value: value.value?,
            })
        }
    }
    impl ::std::convert::From<super::HttpHeader> for HttpHeader {
        fn from(value: super::HttpHeader) -> Self {
            Self {
                meta: Ok(value.meta),
                name: Ok(value.name),
                value: Ok(value.value),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct ImageContent {
        annotations:
            ::std::result::Result<::std::option::Option<super::Annotations>, ::std::string::String>,
        data: ::std::result::Result<::std::string::String, ::std::string::String>,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        mime_type: ::std::result::Result<::std::string::String, ::std::string::String>,
        uri: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for ImageContent {
        fn default() -> Self {
            Self {
                annotations: Ok(Default::default()),
                data: Err("no value supplied for data".to_string()),
                meta: Ok(Default::default()),
                mime_type: Err("no value supplied for mime_type".to_string()),
                uri: Ok(Default::default()),
            }
        }
    }
    impl ImageContent {
        pub fn annotations<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::Annotations>>,
            T::Error: ::std::fmt::Display,
        {
            self.annotations = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for annotations: {e}"));
            self
        }
        pub fn data<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.data = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for data: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn mime_type<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.mime_type = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for mime_type: {e}"));
            self
        }
        pub fn uri<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.uri = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for uri: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<ImageContent> for super::ImageContent {
        type Error = super::error::ConversionError;
        fn try_from(
            value: ImageContent,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                annotations: value.annotations?,
                data: value.data?,
                meta: value.meta?,
                mime_type: value.mime_type?,
                uri: value.uri?,
            })
        }
    }
    impl ::std::convert::From<super::ImageContent> for ImageContent {
        fn from(value: super::ImageContent) -> Self {
            Self {
                annotations: Ok(value.annotations),
                data: Ok(value.data),
                meta: Ok(value.meta),
                mime_type: Ok(value.mime_type),
                uri: Ok(value.uri),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct Implementation {
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        name: ::std::result::Result<::std::string::String, ::std::string::String>,
        title: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
        version: ::std::result::Result<::std::string::String, ::std::string::String>,
    }
    impl ::std::default::Default for Implementation {
        fn default() -> Self {
            Self {
                meta: Ok(Default::default()),
                name: Err("no value supplied for name".to_string()),
                title: Ok(Default::default()),
                version: Err("no value supplied for version".to_string()),
            }
        }
    }
    impl Implementation {
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn name<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.name = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for name: {e}"));
            self
        }
        pub fn title<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.title = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for title: {e}"));
            self
        }
        pub fn version<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.version = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for version: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<Implementation> for super::Implementation {
        type Error = super::error::ConversionError;
        fn try_from(
            value: Implementation,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                meta: value.meta?,
                name: value.name?,
                title: value.title?,
                version: value.version?,
            })
        }
    }
    impl ::std::convert::From<super::Implementation> for Implementation {
        fn from(value: super::Implementation) -> Self {
            Self {
                meta: Ok(value.meta),
                name: Ok(value.name),
                title: Ok(value.title),
                version: Ok(value.version),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct InitializeRequest {
        client_capabilities:
            ::std::result::Result<super::ClientCapabilities, ::std::string::String>,
        client_info: ::std::result::Result<
            ::std::option::Option<super::Implementation>,
            ::std::string::String,
        >,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        protocol_version: ::std::result::Result<super::ProtocolVersion, ::std::string::String>,
    }
    impl ::std::default::Default for InitializeRequest {
        fn default() -> Self {
            Self {
                client_capabilities: Ok(super::defaults::initialize_request_client_capabilities()),
                client_info: Ok(Default::default()),
                meta: Ok(Default::default()),
                protocol_version: Err("no value supplied for protocol_version".to_string()),
            }
        }
    }
    impl InitializeRequest {
        pub fn client_capabilities<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::ClientCapabilities>,
            T::Error: ::std::fmt::Display,
        {
            self.client_capabilities = value.try_into().map_err(|e| {
                format!("error converting supplied value for client_capabilities: {e}")
            });
            self
        }
        pub fn client_info<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::Implementation>>,
            T::Error: ::std::fmt::Display,
        {
            self.client_info = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for client_info: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn protocol_version<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::ProtocolVersion>,
            T::Error: ::std::fmt::Display,
        {
            self.protocol_version = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for protocol_version: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<InitializeRequest> for super::InitializeRequest {
        type Error = super::error::ConversionError;
        fn try_from(
            value: InitializeRequest,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                client_capabilities: value.client_capabilities?,
                client_info: value.client_info?,
                meta: value.meta?,
                protocol_version: value.protocol_version?,
            })
        }
    }
    impl ::std::convert::From<super::InitializeRequest> for InitializeRequest {
        fn from(value: super::InitializeRequest) -> Self {
            Self {
                client_capabilities: Ok(value.client_capabilities),
                client_info: Ok(value.client_info),
                meta: Ok(value.meta),
                protocol_version: Ok(value.protocol_version),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct InitializeResponse {
        agent_capabilities: ::std::result::Result<super::AgentCapabilities, ::std::string::String>,
        agent_info: ::std::result::Result<
            ::std::option::Option<super::Implementation>,
            ::std::string::String,
        >,
        auth_methods:
            ::std::result::Result<::std::vec::Vec<super::AuthMethod>, ::std::string::String>,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        protocol_version: ::std::result::Result<super::ProtocolVersion, ::std::string::String>,
    }
    impl ::std::default::Default for InitializeResponse {
        fn default() -> Self {
            Self {
                agent_capabilities: Ok(super::defaults::initialize_response_agent_capabilities()),
                agent_info: Ok(Default::default()),
                auth_methods: Ok(Default::default()),
                meta: Ok(Default::default()),
                protocol_version: Err("no value supplied for protocol_version".to_string()),
            }
        }
    }
    impl InitializeResponse {
        pub fn agent_capabilities<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::AgentCapabilities>,
            T::Error: ::std::fmt::Display,
        {
            self.agent_capabilities = value.try_into().map_err(|e| {
                format!("error converting supplied value for agent_capabilities: {e}")
            });
            self
        }
        pub fn agent_info<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::Implementation>>,
            T::Error: ::std::fmt::Display,
        {
            self.agent_info = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for agent_info: {e}"));
            self
        }
        pub fn auth_methods<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::vec::Vec<super::AuthMethod>>,
            T::Error: ::std::fmt::Display,
        {
            self.auth_methods = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for auth_methods: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn protocol_version<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::ProtocolVersion>,
            T::Error: ::std::fmt::Display,
        {
            self.protocol_version = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for protocol_version: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<InitializeResponse> for super::InitializeResponse {
        type Error = super::error::ConversionError;
        fn try_from(
            value: InitializeResponse,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                agent_capabilities: value.agent_capabilities?,
                agent_info: value.agent_info?,
                auth_methods: value.auth_methods?,
                meta: value.meta?,
                protocol_version: value.protocol_version?,
            })
        }
    }
    impl ::std::convert::From<super::InitializeResponse> for InitializeResponse {
        fn from(value: super::InitializeResponse) -> Self {
            Self {
                agent_capabilities: Ok(value.agent_capabilities),
                agent_info: Ok(value.agent_info),
                auth_methods: Ok(value.auth_methods),
                meta: Ok(value.meta),
                protocol_version: Ok(value.protocol_version),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct KillTerminalRequest {
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        session_id: ::std::result::Result<super::SessionId, ::std::string::String>,
        terminal_id: ::std::result::Result<super::TerminalId, ::std::string::String>,
    }
    impl ::std::default::Default for KillTerminalRequest {
        fn default() -> Self {
            Self {
                meta: Ok(Default::default()),
                session_id: Err("no value supplied for session_id".to_string()),
                terminal_id: Err("no value supplied for terminal_id".to_string()),
            }
        }
    }
    impl KillTerminalRequest {
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn session_id<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::SessionId>,
            T::Error: ::std::fmt::Display,
        {
            self.session_id = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for session_id: {e}"));
            self
        }
        pub fn terminal_id<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::TerminalId>,
            T::Error: ::std::fmt::Display,
        {
            self.terminal_id = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for terminal_id: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<KillTerminalRequest> for super::KillTerminalRequest {
        type Error = super::error::ConversionError;
        fn try_from(
            value: KillTerminalRequest,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                meta: value.meta?,
                session_id: value.session_id?,
                terminal_id: value.terminal_id?,
            })
        }
    }
    impl ::std::convert::From<super::KillTerminalRequest> for KillTerminalRequest {
        fn from(value: super::KillTerminalRequest) -> Self {
            Self {
                meta: Ok(value.meta),
                session_id: Ok(value.session_id),
                terminal_id: Ok(value.terminal_id),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct KillTerminalResponse {
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for KillTerminalResponse {
        fn default() -> Self {
            Self {
                meta: Ok(Default::default()),
            }
        }
    }
    impl KillTerminalResponse {
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<KillTerminalResponse> for super::KillTerminalResponse {
        type Error = super::error::ConversionError;
        fn try_from(
            value: KillTerminalResponse,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self { meta: value.meta? })
        }
    }
    impl ::std::convert::From<super::KillTerminalResponse> for KillTerminalResponse {
        fn from(value: super::KillTerminalResponse) -> Self {
            Self {
                meta: Ok(value.meta),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct ListSessionsRequest {
        cursor: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
        cwd: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for ListSessionsRequest {
        fn default() -> Self {
            Self {
                cursor: Ok(Default::default()),
                cwd: Ok(Default::default()),
                meta: Ok(Default::default()),
            }
        }
    }
    impl ListSessionsRequest {
        pub fn cursor<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.cursor = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for cursor: {e}"));
            self
        }
        pub fn cwd<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.cwd = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for cwd: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<ListSessionsRequest> for super::ListSessionsRequest {
        type Error = super::error::ConversionError;
        fn try_from(
            value: ListSessionsRequest,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                cursor: value.cursor?,
                cwd: value.cwd?,
                meta: value.meta?,
            })
        }
    }
    impl ::std::convert::From<super::ListSessionsRequest> for ListSessionsRequest {
        fn from(value: super::ListSessionsRequest) -> Self {
            Self {
                cursor: Ok(value.cursor),
                cwd: Ok(value.cwd),
                meta: Ok(value.meta),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct ListSessionsResponse {
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        next_cursor: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
        sessions: ::std::result::Result<::std::vec::Vec<super::SessionInfo>, ::std::string::String>,
    }
    impl ::std::default::Default for ListSessionsResponse {
        fn default() -> Self {
            Self {
                meta: Ok(Default::default()),
                next_cursor: Ok(Default::default()),
                sessions: Err("no value supplied for sessions".to_string()),
            }
        }
    }
    impl ListSessionsResponse {
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn next_cursor<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.next_cursor = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for next_cursor: {e}"));
            self
        }
        pub fn sessions<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::vec::Vec<super::SessionInfo>>,
            T::Error: ::std::fmt::Display,
        {
            self.sessions = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for sessions: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<ListSessionsResponse> for super::ListSessionsResponse {
        type Error = super::error::ConversionError;
        fn try_from(
            value: ListSessionsResponse,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                meta: value.meta?,
                next_cursor: value.next_cursor?,
                sessions: value.sessions?,
            })
        }
    }
    impl ::std::convert::From<super::ListSessionsResponse> for ListSessionsResponse {
        fn from(value: super::ListSessionsResponse) -> Self {
            Self {
                meta: Ok(value.meta),
                next_cursor: Ok(value.next_cursor),
                sessions: Ok(value.sessions),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct LoadSessionRequest {
        additional_directories:
            ::std::result::Result<::std::vec::Vec<::std::string::String>, ::std::string::String>,
        cwd: ::std::result::Result<::std::string::String, ::std::string::String>,
        mcp_servers:
            ::std::result::Result<::std::vec::Vec<super::McpServer>, ::std::string::String>,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        session_id: ::std::result::Result<super::SessionId, ::std::string::String>,
    }
    impl ::std::default::Default for LoadSessionRequest {
        fn default() -> Self {
            Self {
                additional_directories: Ok(Default::default()),
                cwd: Err("no value supplied for cwd".to_string()),
                mcp_servers: Err("no value supplied for mcp_servers".to_string()),
                meta: Ok(Default::default()),
                session_id: Err("no value supplied for session_id".to_string()),
            }
        }
    }
    impl LoadSessionRequest {
        pub fn additional_directories<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::vec::Vec<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.additional_directories = value.try_into().map_err(|e| {
                format!("error converting supplied value for additional_directories: {e}")
            });
            self
        }
        pub fn cwd<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.cwd = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for cwd: {e}"));
            self
        }
        pub fn mcp_servers<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::vec::Vec<super::McpServer>>,
            T::Error: ::std::fmt::Display,
        {
            self.mcp_servers = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for mcp_servers: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn session_id<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::SessionId>,
            T::Error: ::std::fmt::Display,
        {
            self.session_id = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for session_id: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<LoadSessionRequest> for super::LoadSessionRequest {
        type Error = super::error::ConversionError;
        fn try_from(
            value: LoadSessionRequest,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                additional_directories: value.additional_directories?,
                cwd: value.cwd?,
                mcp_servers: value.mcp_servers?,
                meta: value.meta?,
                session_id: value.session_id?,
            })
        }
    }
    impl ::std::convert::From<super::LoadSessionRequest> for LoadSessionRequest {
        fn from(value: super::LoadSessionRequest) -> Self {
            Self {
                additional_directories: Ok(value.additional_directories),
                cwd: Ok(value.cwd),
                mcp_servers: Ok(value.mcp_servers),
                meta: Ok(value.meta),
                session_id: Ok(value.session_id),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct LoadSessionResponse {
        config_options: ::std::result::Result<
            ::std::option::Option<::std::vec::Vec<super::SessionConfigOption>>,
            ::std::string::String,
        >,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        modes: ::std::result::Result<
            ::std::option::Option<super::SessionModeState>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for LoadSessionResponse {
        fn default() -> Self {
            Self {
                config_options: Ok(Default::default()),
                meta: Ok(Default::default()),
                modes: Ok(Default::default()),
            }
        }
    }
    impl LoadSessionResponse {
        pub fn config_options<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<::std::vec::Vec<super::SessionConfigOption>>,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.config_options = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for config_options: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn modes<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::SessionModeState>>,
            T::Error: ::std::fmt::Display,
        {
            self.modes = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for modes: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<LoadSessionResponse> for super::LoadSessionResponse {
        type Error = super::error::ConversionError;
        fn try_from(
            value: LoadSessionResponse,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                config_options: value.config_options?,
                meta: value.meta?,
                modes: value.modes?,
            })
        }
    }
    impl ::std::convert::From<super::LoadSessionResponse> for LoadSessionResponse {
        fn from(value: super::LoadSessionResponse) -> Self {
            Self {
                config_options: Ok(value.config_options),
                meta: Ok(value.meta),
                modes: Ok(value.modes),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct LogoutCapabilities {
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for LogoutCapabilities {
        fn default() -> Self {
            Self {
                meta: Ok(Default::default()),
            }
        }
    }
    impl LogoutCapabilities {
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<LogoutCapabilities> for super::LogoutCapabilities {
        type Error = super::error::ConversionError;
        fn try_from(
            value: LogoutCapabilities,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self { meta: value.meta? })
        }
    }
    impl ::std::convert::From<super::LogoutCapabilities> for LogoutCapabilities {
        fn from(value: super::LogoutCapabilities) -> Self {
            Self {
                meta: Ok(value.meta),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct LogoutRequest {
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for LogoutRequest {
        fn default() -> Self {
            Self {
                meta: Ok(Default::default()),
            }
        }
    }
    impl LogoutRequest {
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<LogoutRequest> for super::LogoutRequest {
        type Error = super::error::ConversionError;
        fn try_from(
            value: LogoutRequest,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self { meta: value.meta? })
        }
    }
    impl ::std::convert::From<super::LogoutRequest> for LogoutRequest {
        fn from(value: super::LogoutRequest) -> Self {
            Self {
                meta: Ok(value.meta),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct LogoutResponse {
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for LogoutResponse {
        fn default() -> Self {
            Self {
                meta: Ok(Default::default()),
            }
        }
    }
    impl LogoutResponse {
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<LogoutResponse> for super::LogoutResponse {
        type Error = super::error::ConversionError;
        fn try_from(
            value: LogoutResponse,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self { meta: value.meta? })
        }
    }
    impl ::std::convert::From<super::LogoutResponse> for LogoutResponse {
        fn from(value: super::LogoutResponse) -> Self {
            Self {
                meta: Ok(value.meta),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct McpCapabilities {
        http: ::std::result::Result<bool, ::std::string::String>,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        sse: ::std::result::Result<bool, ::std::string::String>,
    }
    impl ::std::default::Default for McpCapabilities {
        fn default() -> Self {
            Self {
                http: Ok(Default::default()),
                meta: Ok(Default::default()),
                sse: Ok(Default::default()),
            }
        }
    }
    impl McpCapabilities {
        pub fn http<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<bool>,
            T::Error: ::std::fmt::Display,
        {
            self.http = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for http: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn sse<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<bool>,
            T::Error: ::std::fmt::Display,
        {
            self.sse = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for sse: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<McpCapabilities> for super::McpCapabilities {
        type Error = super::error::ConversionError;
        fn try_from(
            value: McpCapabilities,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                http: value.http?,
                meta: value.meta?,
                sse: value.sse?,
            })
        }
    }
    impl ::std::convert::From<super::McpCapabilities> for McpCapabilities {
        fn from(value: super::McpCapabilities) -> Self {
            Self {
                http: Ok(value.http),
                meta: Ok(value.meta),
                sse: Ok(value.sse),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct McpServer {
        subtype_0: ::std::result::Result<
            ::std::option::Option<super::McpServerSubtype0>,
            ::std::string::String,
        >,
        subtype_1: ::std::result::Result<
            ::std::option::Option<super::McpServerSubtype1>,
            ::std::string::String,
        >,
        subtype_2: ::std::result::Result<
            ::std::option::Option<super::McpServerStdio>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for McpServer {
        fn default() -> Self {
            Self {
                subtype_0: Ok(Default::default()),
                subtype_1: Ok(Default::default()),
                subtype_2: Ok(Default::default()),
            }
        }
    }
    impl McpServer {
        pub fn subtype_0<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::McpServerSubtype0>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_0 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_0: {e}"));
            self
        }
        pub fn subtype_1<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::McpServerSubtype1>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_1 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_1: {e}"));
            self
        }
        pub fn subtype_2<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::McpServerStdio>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_2 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_2: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<McpServer> for super::McpServer {
        type Error = super::error::ConversionError;
        fn try_from(
            value: McpServer,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                subtype_0: value.subtype_0?,
                subtype_1: value.subtype_1?,
                subtype_2: value.subtype_2?,
            })
        }
    }
    impl ::std::convert::From<super::McpServer> for McpServer {
        fn from(value: super::McpServer) -> Self {
            Self {
                subtype_0: Ok(value.subtype_0),
                subtype_1: Ok(value.subtype_1),
                subtype_2: Ok(value.subtype_2),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct McpServerHttp {
        headers: ::std::result::Result<::std::vec::Vec<super::HttpHeader>, ::std::string::String>,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        name: ::std::result::Result<::std::string::String, ::std::string::String>,
        url: ::std::result::Result<::std::string::String, ::std::string::String>,
    }
    impl ::std::default::Default for McpServerHttp {
        fn default() -> Self {
            Self {
                headers: Err("no value supplied for headers".to_string()),
                meta: Ok(Default::default()),
                name: Err("no value supplied for name".to_string()),
                url: Err("no value supplied for url".to_string()),
            }
        }
    }
    impl McpServerHttp {
        pub fn headers<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::vec::Vec<super::HttpHeader>>,
            T::Error: ::std::fmt::Display,
        {
            self.headers = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for headers: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn name<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.name = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for name: {e}"));
            self
        }
        pub fn url<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.url = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for url: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<McpServerHttp> for super::McpServerHttp {
        type Error = super::error::ConversionError;
        fn try_from(
            value: McpServerHttp,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                headers: value.headers?,
                meta: value.meta?,
                name: value.name?,
                url: value.url?,
            })
        }
    }
    impl ::std::convert::From<super::McpServerHttp> for McpServerHttp {
        fn from(value: super::McpServerHttp) -> Self {
            Self {
                headers: Ok(value.headers),
                meta: Ok(value.meta),
                name: Ok(value.name),
                url: Ok(value.url),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct McpServerSse {
        headers: ::std::result::Result<::std::vec::Vec<super::HttpHeader>, ::std::string::String>,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        name: ::std::result::Result<::std::string::String, ::std::string::String>,
        url: ::std::result::Result<::std::string::String, ::std::string::String>,
    }
    impl ::std::default::Default for McpServerSse {
        fn default() -> Self {
            Self {
                headers: Err("no value supplied for headers".to_string()),
                meta: Ok(Default::default()),
                name: Err("no value supplied for name".to_string()),
                url: Err("no value supplied for url".to_string()),
            }
        }
    }
    impl McpServerSse {
        pub fn headers<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::vec::Vec<super::HttpHeader>>,
            T::Error: ::std::fmt::Display,
        {
            self.headers = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for headers: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn name<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.name = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for name: {e}"));
            self
        }
        pub fn url<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.url = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for url: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<McpServerSse> for super::McpServerSse {
        type Error = super::error::ConversionError;
        fn try_from(
            value: McpServerSse,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                headers: value.headers?,
                meta: value.meta?,
                name: value.name?,
                url: value.url?,
            })
        }
    }
    impl ::std::convert::From<super::McpServerSse> for McpServerSse {
        fn from(value: super::McpServerSse) -> Self {
            Self {
                headers: Ok(value.headers),
                meta: Ok(value.meta),
                name: Ok(value.name),
                url: Ok(value.url),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct McpServerStdio {
        args: ::std::result::Result<::std::vec::Vec<::std::string::String>, ::std::string::String>,
        command: ::std::result::Result<::std::string::String, ::std::string::String>,
        env: ::std::result::Result<::std::vec::Vec<super::EnvVariable>, ::std::string::String>,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        name: ::std::result::Result<::std::string::String, ::std::string::String>,
    }
    impl ::std::default::Default for McpServerStdio {
        fn default() -> Self {
            Self {
                args: Err("no value supplied for args".to_string()),
                command: Err("no value supplied for command".to_string()),
                env: Err("no value supplied for env".to_string()),
                meta: Ok(Default::default()),
                name: Err("no value supplied for name".to_string()),
            }
        }
    }
    impl McpServerStdio {
        pub fn args<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::vec::Vec<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.args = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for args: {e}"));
            self
        }
        pub fn command<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.command = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for command: {e}"));
            self
        }
        pub fn env<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::vec::Vec<super::EnvVariable>>,
            T::Error: ::std::fmt::Display,
        {
            self.env = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for env: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn name<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.name = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for name: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<McpServerStdio> for super::McpServerStdio {
        type Error = super::error::ConversionError;
        fn try_from(
            value: McpServerStdio,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                args: value.args?,
                command: value.command?,
                env: value.env?,
                meta: value.meta?,
                name: value.name?,
            })
        }
    }
    impl ::std::convert::From<super::McpServerStdio> for McpServerStdio {
        fn from(value: super::McpServerStdio) -> Self {
            Self {
                args: Ok(value.args),
                command: Ok(value.command),
                env: Ok(value.env),
                meta: Ok(value.meta),
                name: Ok(value.name),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct McpServerSubtype0 {
        headers: ::std::result::Result<::std::vec::Vec<super::HttpHeader>, ::std::string::String>,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        name: ::std::result::Result<::std::string::String, ::std::string::String>,
        type_: ::std::result::Result<::std::string::String, ::std::string::String>,
        url: ::std::result::Result<::std::string::String, ::std::string::String>,
    }
    impl ::std::default::Default for McpServerSubtype0 {
        fn default() -> Self {
            Self {
                headers: Err("no value supplied for headers".to_string()),
                meta: Ok(Default::default()),
                name: Err("no value supplied for name".to_string()),
                type_: Err("no value supplied for type_".to_string()),
                url: Err("no value supplied for url".to_string()),
            }
        }
    }
    impl McpServerSubtype0 {
        pub fn headers<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::vec::Vec<super::HttpHeader>>,
            T::Error: ::std::fmt::Display,
        {
            self.headers = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for headers: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn name<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.name = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for name: {e}"));
            self
        }
        pub fn type_<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.type_ = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for type_: {e}"));
            self
        }
        pub fn url<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.url = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for url: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<McpServerSubtype0> for super::McpServerSubtype0 {
        type Error = super::error::ConversionError;
        fn try_from(
            value: McpServerSubtype0,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                headers: value.headers?,
                meta: value.meta?,
                name: value.name?,
                type_: value.type_?,
                url: value.url?,
            })
        }
    }
    impl ::std::convert::From<super::McpServerSubtype0> for McpServerSubtype0 {
        fn from(value: super::McpServerSubtype0) -> Self {
            Self {
                headers: Ok(value.headers),
                meta: Ok(value.meta),
                name: Ok(value.name),
                type_: Ok(value.type_),
                url: Ok(value.url),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct McpServerSubtype1 {
        headers: ::std::result::Result<::std::vec::Vec<super::HttpHeader>, ::std::string::String>,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        name: ::std::result::Result<::std::string::String, ::std::string::String>,
        type_: ::std::result::Result<::std::string::String, ::std::string::String>,
        url: ::std::result::Result<::std::string::String, ::std::string::String>,
    }
    impl ::std::default::Default for McpServerSubtype1 {
        fn default() -> Self {
            Self {
                headers: Err("no value supplied for headers".to_string()),
                meta: Ok(Default::default()),
                name: Err("no value supplied for name".to_string()),
                type_: Err("no value supplied for type_".to_string()),
                url: Err("no value supplied for url".to_string()),
            }
        }
    }
    impl McpServerSubtype1 {
        pub fn headers<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::vec::Vec<super::HttpHeader>>,
            T::Error: ::std::fmt::Display,
        {
            self.headers = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for headers: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn name<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.name = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for name: {e}"));
            self
        }
        pub fn type_<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.type_ = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for type_: {e}"));
            self
        }
        pub fn url<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.url = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for url: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<McpServerSubtype1> for super::McpServerSubtype1 {
        type Error = super::error::ConversionError;
        fn try_from(
            value: McpServerSubtype1,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                headers: value.headers?,
                meta: value.meta?,
                name: value.name?,
                type_: value.type_?,
                url: value.url?,
            })
        }
    }
    impl ::std::convert::From<super::McpServerSubtype1> for McpServerSubtype1 {
        fn from(value: super::McpServerSubtype1) -> Self {
            Self {
                headers: Ok(value.headers),
                meta: Ok(value.meta),
                name: Ok(value.name),
                type_: Ok(value.type_),
                url: Ok(value.url),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct NewSessionRequest {
        additional_directories:
            ::std::result::Result<::std::vec::Vec<::std::string::String>, ::std::string::String>,
        cwd: ::std::result::Result<::std::string::String, ::std::string::String>,
        mcp_servers:
            ::std::result::Result<::std::vec::Vec<super::McpServer>, ::std::string::String>,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for NewSessionRequest {
        fn default() -> Self {
            Self {
                additional_directories: Ok(Default::default()),
                cwd: Err("no value supplied for cwd".to_string()),
                mcp_servers: Err("no value supplied for mcp_servers".to_string()),
                meta: Ok(Default::default()),
            }
        }
    }
    impl NewSessionRequest {
        pub fn additional_directories<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::vec::Vec<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.additional_directories = value.try_into().map_err(|e| {
                format!("error converting supplied value for additional_directories: {e}")
            });
            self
        }
        pub fn cwd<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.cwd = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for cwd: {e}"));
            self
        }
        pub fn mcp_servers<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::vec::Vec<super::McpServer>>,
            T::Error: ::std::fmt::Display,
        {
            self.mcp_servers = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for mcp_servers: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<NewSessionRequest> for super::NewSessionRequest {
        type Error = super::error::ConversionError;
        fn try_from(
            value: NewSessionRequest,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                additional_directories: value.additional_directories?,
                cwd: value.cwd?,
                mcp_servers: value.mcp_servers?,
                meta: value.meta?,
            })
        }
    }
    impl ::std::convert::From<super::NewSessionRequest> for NewSessionRequest {
        fn from(value: super::NewSessionRequest) -> Self {
            Self {
                additional_directories: Ok(value.additional_directories),
                cwd: Ok(value.cwd),
                mcp_servers: Ok(value.mcp_servers),
                meta: Ok(value.meta),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct NewSessionResponse {
        config_options: ::std::result::Result<
            ::std::option::Option<::std::vec::Vec<super::SessionConfigOption>>,
            ::std::string::String,
        >,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        modes: ::std::result::Result<
            ::std::option::Option<super::SessionModeState>,
            ::std::string::String,
        >,
        session_id: ::std::result::Result<super::SessionId, ::std::string::String>,
    }
    impl ::std::default::Default for NewSessionResponse {
        fn default() -> Self {
            Self {
                config_options: Ok(Default::default()),
                meta: Ok(Default::default()),
                modes: Ok(Default::default()),
                session_id: Err("no value supplied for session_id".to_string()),
            }
        }
    }
    impl NewSessionResponse {
        pub fn config_options<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<::std::vec::Vec<super::SessionConfigOption>>,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.config_options = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for config_options: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn modes<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::SessionModeState>>,
            T::Error: ::std::fmt::Display,
        {
            self.modes = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for modes: {e}"));
            self
        }
        pub fn session_id<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::SessionId>,
            T::Error: ::std::fmt::Display,
        {
            self.session_id = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for session_id: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<NewSessionResponse> for super::NewSessionResponse {
        type Error = super::error::ConversionError;
        fn try_from(
            value: NewSessionResponse,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                config_options: value.config_options?,
                meta: value.meta?,
                modes: value.modes?,
                session_id: value.session_id?,
            })
        }
    }
    impl ::std::convert::From<super::NewSessionResponse> for NewSessionResponse {
        fn from(value: super::NewSessionResponse) -> Self {
            Self {
                config_options: Ok(value.config_options),
                meta: Ok(value.meta),
                modes: Ok(value.modes),
                session_id: Ok(value.session_id),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct PermissionOption {
        kind: ::std::result::Result<super::PermissionOptionKind, ::std::string::String>,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        name: ::std::result::Result<::std::string::String, ::std::string::String>,
        option_id: ::std::result::Result<super::PermissionOptionId, ::std::string::String>,
    }
    impl ::std::default::Default for PermissionOption {
        fn default() -> Self {
            Self {
                kind: Err("no value supplied for kind".to_string()),
                meta: Ok(Default::default()),
                name: Err("no value supplied for name".to_string()),
                option_id: Err("no value supplied for option_id".to_string()),
            }
        }
    }
    impl PermissionOption {
        pub fn kind<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::PermissionOptionKind>,
            T::Error: ::std::fmt::Display,
        {
            self.kind = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for kind: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn name<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.name = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for name: {e}"));
            self
        }
        pub fn option_id<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::PermissionOptionId>,
            T::Error: ::std::fmt::Display,
        {
            self.option_id = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for option_id: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<PermissionOption> for super::PermissionOption {
        type Error = super::error::ConversionError;
        fn try_from(
            value: PermissionOption,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                kind: value.kind?,
                meta: value.meta?,
                name: value.name?,
                option_id: value.option_id?,
            })
        }
    }
    impl ::std::convert::From<super::PermissionOption> for PermissionOption {
        fn from(value: super::PermissionOption) -> Self {
            Self {
                kind: Ok(value.kind),
                meta: Ok(value.meta),
                name: Ok(value.name),
                option_id: Ok(value.option_id),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct Plan {
        entries: ::std::result::Result<::std::vec::Vec<super::PlanEntry>, ::std::string::String>,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for Plan {
        fn default() -> Self {
            Self {
                entries: Err("no value supplied for entries".to_string()),
                meta: Ok(Default::default()),
            }
        }
    }
    impl Plan {
        pub fn entries<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::vec::Vec<super::PlanEntry>>,
            T::Error: ::std::fmt::Display,
        {
            self.entries = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for entries: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<Plan> for super::Plan {
        type Error = super::error::ConversionError;
        fn try_from(value: Plan) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                entries: value.entries?,
                meta: value.meta?,
            })
        }
    }
    impl ::std::convert::From<super::Plan> for Plan {
        fn from(value: super::Plan) -> Self {
            Self {
                entries: Ok(value.entries),
                meta: Ok(value.meta),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct PlanEntry {
        content: ::std::result::Result<::std::string::String, ::std::string::String>,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        priority: ::std::result::Result<super::PlanEntryPriority, ::std::string::String>,
        status: ::std::result::Result<super::PlanEntryStatus, ::std::string::String>,
    }
    impl ::std::default::Default for PlanEntry {
        fn default() -> Self {
            Self {
                content: Err("no value supplied for content".to_string()),
                meta: Ok(Default::default()),
                priority: Err("no value supplied for priority".to_string()),
                status: Err("no value supplied for status".to_string()),
            }
        }
    }
    impl PlanEntry {
        pub fn content<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.content = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for content: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn priority<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::PlanEntryPriority>,
            T::Error: ::std::fmt::Display,
        {
            self.priority = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for priority: {e}"));
            self
        }
        pub fn status<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::PlanEntryStatus>,
            T::Error: ::std::fmt::Display,
        {
            self.status = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for status: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<PlanEntry> for super::PlanEntry {
        type Error = super::error::ConversionError;
        fn try_from(
            value: PlanEntry,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                content: value.content?,
                meta: value.meta?,
                priority: value.priority?,
                status: value.status?,
            })
        }
    }
    impl ::std::convert::From<super::PlanEntry> for PlanEntry {
        fn from(value: super::PlanEntry) -> Self {
            Self {
                content: Ok(value.content),
                meta: Ok(value.meta),
                priority: Ok(value.priority),
                status: Ok(value.status),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct PromptCapabilities {
        audio: ::std::result::Result<bool, ::std::string::String>,
        embedded_context: ::std::result::Result<bool, ::std::string::String>,
        image: ::std::result::Result<bool, ::std::string::String>,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for PromptCapabilities {
        fn default() -> Self {
            Self {
                audio: Ok(Default::default()),
                embedded_context: Ok(Default::default()),
                image: Ok(Default::default()),
                meta: Ok(Default::default()),
            }
        }
    }
    impl PromptCapabilities {
        pub fn audio<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<bool>,
            T::Error: ::std::fmt::Display,
        {
            self.audio = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for audio: {e}"));
            self
        }
        pub fn embedded_context<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<bool>,
            T::Error: ::std::fmt::Display,
        {
            self.embedded_context = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for embedded_context: {e}"));
            self
        }
        pub fn image<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<bool>,
            T::Error: ::std::fmt::Display,
        {
            self.image = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for image: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<PromptCapabilities> for super::PromptCapabilities {
        type Error = super::error::ConversionError;
        fn try_from(
            value: PromptCapabilities,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                audio: value.audio?,
                embedded_context: value.embedded_context?,
                image: value.image?,
                meta: value.meta?,
            })
        }
    }
    impl ::std::convert::From<super::PromptCapabilities> for PromptCapabilities {
        fn from(value: super::PromptCapabilities) -> Self {
            Self {
                audio: Ok(value.audio),
                embedded_context: Ok(value.embedded_context),
                image: Ok(value.image),
                meta: Ok(value.meta),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct PromptRequest {
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        prompt: ::std::result::Result<::std::vec::Vec<super::ContentBlock>, ::std::string::String>,
        session_id: ::std::result::Result<super::SessionId, ::std::string::String>,
    }
    impl ::std::default::Default for PromptRequest {
        fn default() -> Self {
            Self {
                meta: Ok(Default::default()),
                prompt: Err("no value supplied for prompt".to_string()),
                session_id: Err("no value supplied for session_id".to_string()),
            }
        }
    }
    impl PromptRequest {
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn prompt<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::vec::Vec<super::ContentBlock>>,
            T::Error: ::std::fmt::Display,
        {
            self.prompt = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for prompt: {e}"));
            self
        }
        pub fn session_id<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::SessionId>,
            T::Error: ::std::fmt::Display,
        {
            self.session_id = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for session_id: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<PromptRequest> for super::PromptRequest {
        type Error = super::error::ConversionError;
        fn try_from(
            value: PromptRequest,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                meta: value.meta?,
                prompt: value.prompt?,
                session_id: value.session_id?,
            })
        }
    }
    impl ::std::convert::From<super::PromptRequest> for PromptRequest {
        fn from(value: super::PromptRequest) -> Self {
            Self {
                meta: Ok(value.meta),
                prompt: Ok(value.prompt),
                session_id: Ok(value.session_id),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct PromptResponse {
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        stop_reason: ::std::result::Result<super::StopReason, ::std::string::String>,
    }
    impl ::std::default::Default for PromptResponse {
        fn default() -> Self {
            Self {
                meta: Ok(Default::default()),
                stop_reason: Err("no value supplied for stop_reason".to_string()),
            }
        }
    }
    impl PromptResponse {
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn stop_reason<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::StopReason>,
            T::Error: ::std::fmt::Display,
        {
            self.stop_reason = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for stop_reason: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<PromptResponse> for super::PromptResponse {
        type Error = super::error::ConversionError;
        fn try_from(
            value: PromptResponse,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                meta: value.meta?,
                stop_reason: value.stop_reason?,
            })
        }
    }
    impl ::std::convert::From<super::PromptResponse> for PromptResponse {
        fn from(value: super::PromptResponse) -> Self {
            Self {
                meta: Ok(value.meta),
                stop_reason: Ok(value.stop_reason),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct ProtocolLevel {
        jsonrpc: ::std::result::Result<super::ProtocolLevelJsonrpc, ::std::string::String>,
        method: ::std::result::Result<::std::string::String, ::std::string::String>,
        params: ::std::result::Result<
            ::std::option::Option<super::CancelRequestNotification>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for ProtocolLevel {
        fn default() -> Self {
            Self {
                jsonrpc: Err("no value supplied for jsonrpc".to_string()),
                method: Err("no value supplied for method".to_string()),
                params: Ok(Default::default()),
            }
        }
    }
    impl ProtocolLevel {
        pub fn jsonrpc<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::ProtocolLevelJsonrpc>,
            T::Error: ::std::fmt::Display,
        {
            self.jsonrpc = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for jsonrpc: {e}"));
            self
        }
        pub fn method<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.method = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for method: {e}"));
            self
        }
        pub fn params<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::CancelRequestNotification>>,
            T::Error: ::std::fmt::Display,
        {
            self.params = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for params: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<ProtocolLevel> for super::ProtocolLevel {
        type Error = super::error::ConversionError;
        fn try_from(
            value: ProtocolLevel,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                jsonrpc: value.jsonrpc?,
                method: value.method?,
                params: value.params?,
            })
        }
    }
    impl ::std::convert::From<super::ProtocolLevel> for ProtocolLevel {
        fn from(value: super::ProtocolLevel) -> Self {
            Self {
                jsonrpc: Ok(value.jsonrpc),
                method: Ok(value.method),
                params: Ok(value.params),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct ReadTextFileRequest {
        limit: ::std::result::Result<::std::option::Option<u32>, ::std::string::String>,
        line: ::std::result::Result<::std::option::Option<u32>, ::std::string::String>,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        path: ::std::result::Result<::std::string::String, ::std::string::String>,
        session_id: ::std::result::Result<super::SessionId, ::std::string::String>,
    }
    impl ::std::default::Default for ReadTextFileRequest {
        fn default() -> Self {
            Self {
                limit: Ok(Default::default()),
                line: Ok(Default::default()),
                meta: Ok(Default::default()),
                path: Err("no value supplied for path".to_string()),
                session_id: Err("no value supplied for session_id".to_string()),
            }
        }
    }
    impl ReadTextFileRequest {
        pub fn limit<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<u32>>,
            T::Error: ::std::fmt::Display,
        {
            self.limit = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for limit: {e}"));
            self
        }
        pub fn line<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<u32>>,
            T::Error: ::std::fmt::Display,
        {
            self.line = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for line: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn path<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.path = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for path: {e}"));
            self
        }
        pub fn session_id<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::SessionId>,
            T::Error: ::std::fmt::Display,
        {
            self.session_id = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for session_id: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<ReadTextFileRequest> for super::ReadTextFileRequest {
        type Error = super::error::ConversionError;
        fn try_from(
            value: ReadTextFileRequest,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                limit: value.limit?,
                line: value.line?,
                meta: value.meta?,
                path: value.path?,
                session_id: value.session_id?,
            })
        }
    }
    impl ::std::convert::From<super::ReadTextFileRequest> for ReadTextFileRequest {
        fn from(value: super::ReadTextFileRequest) -> Self {
            Self {
                limit: Ok(value.limit),
                line: Ok(value.line),
                meta: Ok(value.meta),
                path: Ok(value.path),
                session_id: Ok(value.session_id),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct ReadTextFileResponse {
        content: ::std::result::Result<::std::string::String, ::std::string::String>,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for ReadTextFileResponse {
        fn default() -> Self {
            Self {
                content: Err("no value supplied for content".to_string()),
                meta: Ok(Default::default()),
            }
        }
    }
    impl ReadTextFileResponse {
        pub fn content<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.content = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for content: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<ReadTextFileResponse> for super::ReadTextFileResponse {
        type Error = super::error::ConversionError;
        fn try_from(
            value: ReadTextFileResponse,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                content: value.content?,
                meta: value.meta?,
            })
        }
    }
    impl ::std::convert::From<super::ReadTextFileResponse> for ReadTextFileResponse {
        fn from(value: super::ReadTextFileResponse) -> Self {
            Self {
                content: Ok(value.content),
                meta: Ok(value.meta),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct ReleaseTerminalRequest {
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        session_id: ::std::result::Result<super::SessionId, ::std::string::String>,
        terminal_id: ::std::result::Result<super::TerminalId, ::std::string::String>,
    }
    impl ::std::default::Default for ReleaseTerminalRequest {
        fn default() -> Self {
            Self {
                meta: Ok(Default::default()),
                session_id: Err("no value supplied for session_id".to_string()),
                terminal_id: Err("no value supplied for terminal_id".to_string()),
            }
        }
    }
    impl ReleaseTerminalRequest {
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn session_id<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::SessionId>,
            T::Error: ::std::fmt::Display,
        {
            self.session_id = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for session_id: {e}"));
            self
        }
        pub fn terminal_id<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::TerminalId>,
            T::Error: ::std::fmt::Display,
        {
            self.terminal_id = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for terminal_id: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<ReleaseTerminalRequest> for super::ReleaseTerminalRequest {
        type Error = super::error::ConversionError;
        fn try_from(
            value: ReleaseTerminalRequest,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                meta: value.meta?,
                session_id: value.session_id?,
                terminal_id: value.terminal_id?,
            })
        }
    }
    impl ::std::convert::From<super::ReleaseTerminalRequest> for ReleaseTerminalRequest {
        fn from(value: super::ReleaseTerminalRequest) -> Self {
            Self {
                meta: Ok(value.meta),
                session_id: Ok(value.session_id),
                terminal_id: Ok(value.terminal_id),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct ReleaseTerminalResponse {
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for ReleaseTerminalResponse {
        fn default() -> Self {
            Self {
                meta: Ok(Default::default()),
            }
        }
    }
    impl ReleaseTerminalResponse {
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<ReleaseTerminalResponse> for super::ReleaseTerminalResponse {
        type Error = super::error::ConversionError;
        fn try_from(
            value: ReleaseTerminalResponse,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self { meta: value.meta? })
        }
    }
    impl ::std::convert::From<super::ReleaseTerminalResponse> for ReleaseTerminalResponse {
        fn from(value: super::ReleaseTerminalResponse) -> Self {
            Self {
                meta: Ok(value.meta),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct RequestPermissionRequest {
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        options:
            ::std::result::Result<::std::vec::Vec<super::PermissionOption>, ::std::string::String>,
        session_id: ::std::result::Result<super::SessionId, ::std::string::String>,
        tool_call: ::std::result::Result<super::ToolCallUpdate, ::std::string::String>,
    }
    impl ::std::default::Default for RequestPermissionRequest {
        fn default() -> Self {
            Self {
                meta: Ok(Default::default()),
                options: Err("no value supplied for options".to_string()),
                session_id: Err("no value supplied for session_id".to_string()),
                tool_call: Err("no value supplied for tool_call".to_string()),
            }
        }
    }
    impl RequestPermissionRequest {
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn options<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::vec::Vec<super::PermissionOption>>,
            T::Error: ::std::fmt::Display,
        {
            self.options = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for options: {e}"));
            self
        }
        pub fn session_id<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::SessionId>,
            T::Error: ::std::fmt::Display,
        {
            self.session_id = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for session_id: {e}"));
            self
        }
        pub fn tool_call<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::ToolCallUpdate>,
            T::Error: ::std::fmt::Display,
        {
            self.tool_call = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for tool_call: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<RequestPermissionRequest> for super::RequestPermissionRequest {
        type Error = super::error::ConversionError;
        fn try_from(
            value: RequestPermissionRequest,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                meta: value.meta?,
                options: value.options?,
                session_id: value.session_id?,
                tool_call: value.tool_call?,
            })
        }
    }
    impl ::std::convert::From<super::RequestPermissionRequest> for RequestPermissionRequest {
        fn from(value: super::RequestPermissionRequest) -> Self {
            Self {
                meta: Ok(value.meta),
                options: Ok(value.options),
                session_id: Ok(value.session_id),
                tool_call: Ok(value.tool_call),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct RequestPermissionResponse {
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        outcome: ::std::result::Result<super::RequestPermissionOutcome, ::std::string::String>,
    }
    impl ::std::default::Default for RequestPermissionResponse {
        fn default() -> Self {
            Self {
                meta: Ok(Default::default()),
                outcome: Err("no value supplied for outcome".to_string()),
            }
        }
    }
    impl RequestPermissionResponse {
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn outcome<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::RequestPermissionOutcome>,
            T::Error: ::std::fmt::Display,
        {
            self.outcome = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for outcome: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<RequestPermissionResponse> for super::RequestPermissionResponse {
        type Error = super::error::ConversionError;
        fn try_from(
            value: RequestPermissionResponse,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                meta: value.meta?,
                outcome: value.outcome?,
            })
        }
    }
    impl ::std::convert::From<super::RequestPermissionResponse> for RequestPermissionResponse {
        fn from(value: super::RequestPermissionResponse) -> Self {
            Self {
                meta: Ok(value.meta),
                outcome: Ok(value.outcome),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct ResourceLink {
        annotations:
            ::std::result::Result<::std::option::Option<super::Annotations>, ::std::string::String>,
        description: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        mime_type: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
        name: ::std::result::Result<::std::string::String, ::std::string::String>,
        size: ::std::result::Result<::std::option::Option<i64>, ::std::string::String>,
        title: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
        uri: ::std::result::Result<::std::string::String, ::std::string::String>,
    }
    impl ::std::default::Default for ResourceLink {
        fn default() -> Self {
            Self {
                annotations: Ok(Default::default()),
                description: Ok(Default::default()),
                meta: Ok(Default::default()),
                mime_type: Ok(Default::default()),
                name: Err("no value supplied for name".to_string()),
                size: Ok(Default::default()),
                title: Ok(Default::default()),
                uri: Err("no value supplied for uri".to_string()),
            }
        }
    }
    impl ResourceLink {
        pub fn annotations<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::Annotations>>,
            T::Error: ::std::fmt::Display,
        {
            self.annotations = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for annotations: {e}"));
            self
        }
        pub fn description<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.description = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for description: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn mime_type<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.mime_type = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for mime_type: {e}"));
            self
        }
        pub fn name<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.name = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for name: {e}"));
            self
        }
        pub fn size<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<i64>>,
            T::Error: ::std::fmt::Display,
        {
            self.size = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for size: {e}"));
            self
        }
        pub fn title<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.title = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for title: {e}"));
            self
        }
        pub fn uri<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.uri = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for uri: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<ResourceLink> for super::ResourceLink {
        type Error = super::error::ConversionError;
        fn try_from(
            value: ResourceLink,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                annotations: value.annotations?,
                description: value.description?,
                meta: value.meta?,
                mime_type: value.mime_type?,
                name: value.name?,
                size: value.size?,
                title: value.title?,
                uri: value.uri?,
            })
        }
    }
    impl ::std::convert::From<super::ResourceLink> for ResourceLink {
        fn from(value: super::ResourceLink) -> Self {
            Self {
                annotations: Ok(value.annotations),
                description: Ok(value.description),
                meta: Ok(value.meta),
                mime_type: Ok(value.mime_type),
                name: Ok(value.name),
                size: Ok(value.size),
                title: Ok(value.title),
                uri: Ok(value.uri),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct ResultResult {
        subtype_0: ::std::result::Result<
            ::std::option::Option<super::InitializeResponse>,
            ::std::string::String,
        >,
        subtype_1: ::std::result::Result<
            ::std::option::Option<super::AuthenticateResponse>,
            ::std::string::String,
        >,
        subtype_2: ::std::result::Result<
            ::std::option::Option<super::LogoutResponse>,
            ::std::string::String,
        >,
        subtype_3: ::std::result::Result<
            ::std::option::Option<super::NewSessionResponse>,
            ::std::string::String,
        >,
        subtype_4: ::std::result::Result<
            ::std::option::Option<super::LoadSessionResponse>,
            ::std::string::String,
        >,
        subtype_5: ::std::result::Result<
            ::std::option::Option<super::ListSessionsResponse>,
            ::std::string::String,
        >,
        subtype_6: ::std::result::Result<
            ::std::option::Option<super::DeleteSessionResponse>,
            ::std::string::String,
        >,
        subtype_7: ::std::result::Result<
            ::std::option::Option<super::ResumeSessionResponse>,
            ::std::string::String,
        >,
        subtype_8: ::std::result::Result<
            ::std::option::Option<super::CloseSessionResponse>,
            ::std::string::String,
        >,
        subtype_9: ::std::result::Result<
            ::std::option::Option<super::SetSessionModeResponse>,
            ::std::string::String,
        >,
        subtype_10: ::std::result::Result<
            ::std::option::Option<super::SetSessionConfigOptionResponse>,
            ::std::string::String,
        >,
        subtype_11: ::std::result::Result<
            ::std::option::Option<super::PromptResponse>,
            ::std::string::String,
        >,
        subtype_12:
            ::std::result::Result<::std::option::Option<super::ExtResponse>, ::std::string::String>,
    }
    impl ::std::default::Default for ResultResult {
        fn default() -> Self {
            Self {
                subtype_0: Ok(Default::default()),
                subtype_1: Ok(Default::default()),
                subtype_2: Ok(Default::default()),
                subtype_3: Ok(Default::default()),
                subtype_4: Ok(Default::default()),
                subtype_5: Ok(Default::default()),
                subtype_6: Ok(Default::default()),
                subtype_7: Ok(Default::default()),
                subtype_8: Ok(Default::default()),
                subtype_9: Ok(Default::default()),
                subtype_10: Ok(Default::default()),
                subtype_11: Ok(Default::default()),
                subtype_12: Ok(Default::default()),
            }
        }
    }
    impl ResultResult {
        pub fn subtype_0<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::InitializeResponse>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_0 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_0: {e}"));
            self
        }
        pub fn subtype_1<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::AuthenticateResponse>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_1 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_1: {e}"));
            self
        }
        pub fn subtype_2<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::LogoutResponse>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_2 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_2: {e}"));
            self
        }
        pub fn subtype_3<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::NewSessionResponse>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_3 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_3: {e}"));
            self
        }
        pub fn subtype_4<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::LoadSessionResponse>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_4 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_4: {e}"));
            self
        }
        pub fn subtype_5<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::ListSessionsResponse>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_5 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_5: {e}"));
            self
        }
        pub fn subtype_6<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::DeleteSessionResponse>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_6 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_6: {e}"));
            self
        }
        pub fn subtype_7<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::ResumeSessionResponse>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_7 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_7: {e}"));
            self
        }
        pub fn subtype_8<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::CloseSessionResponse>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_8 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_8: {e}"));
            self
        }
        pub fn subtype_9<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::SetSessionModeResponse>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_9 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_9: {e}"));
            self
        }
        pub fn subtype_10<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<super::SetSessionConfigOptionResponse>,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_10 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_10: {e}"));
            self
        }
        pub fn subtype_11<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::PromptResponse>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_11 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_11: {e}"));
            self
        }
        pub fn subtype_12<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::ExtResponse>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_12 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_12: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<ResultResult> for super::ResultResult {
        type Error = super::error::ConversionError;
        fn try_from(
            value: ResultResult,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                subtype_0: value.subtype_0?,
                subtype_1: value.subtype_1?,
                subtype_2: value.subtype_2?,
                subtype_3: value.subtype_3?,
                subtype_4: value.subtype_4?,
                subtype_5: value.subtype_5?,
                subtype_6: value.subtype_6?,
                subtype_7: value.subtype_7?,
                subtype_8: value.subtype_8?,
                subtype_9: value.subtype_9?,
                subtype_10: value.subtype_10?,
                subtype_11: value.subtype_11?,
                subtype_12: value.subtype_12?,
            })
        }
    }
    impl ::std::convert::From<super::ResultResult> for ResultResult {
        fn from(value: super::ResultResult) -> Self {
            Self {
                subtype_0: Ok(value.subtype_0),
                subtype_1: Ok(value.subtype_1),
                subtype_2: Ok(value.subtype_2),
                subtype_3: Ok(value.subtype_3),
                subtype_4: Ok(value.subtype_4),
                subtype_5: Ok(value.subtype_5),
                subtype_6: Ok(value.subtype_6),
                subtype_7: Ok(value.subtype_7),
                subtype_8: Ok(value.subtype_8),
                subtype_9: Ok(value.subtype_9),
                subtype_10: Ok(value.subtype_10),
                subtype_11: Ok(value.subtype_11),
                subtype_12: Ok(value.subtype_12),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct ResumeSessionRequest {
        additional_directories:
            ::std::result::Result<::std::vec::Vec<::std::string::String>, ::std::string::String>,
        cwd: ::std::result::Result<::std::string::String, ::std::string::String>,
        mcp_servers:
            ::std::result::Result<::std::vec::Vec<super::McpServer>, ::std::string::String>,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        session_id: ::std::result::Result<super::SessionId, ::std::string::String>,
    }
    impl ::std::default::Default for ResumeSessionRequest {
        fn default() -> Self {
            Self {
                additional_directories: Ok(Default::default()),
                cwd: Err("no value supplied for cwd".to_string()),
                mcp_servers: Ok(Default::default()),
                meta: Ok(Default::default()),
                session_id: Err("no value supplied for session_id".to_string()),
            }
        }
    }
    impl ResumeSessionRequest {
        pub fn additional_directories<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::vec::Vec<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.additional_directories = value.try_into().map_err(|e| {
                format!("error converting supplied value for additional_directories: {e}")
            });
            self
        }
        pub fn cwd<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.cwd = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for cwd: {e}"));
            self
        }
        pub fn mcp_servers<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::vec::Vec<super::McpServer>>,
            T::Error: ::std::fmt::Display,
        {
            self.mcp_servers = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for mcp_servers: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn session_id<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::SessionId>,
            T::Error: ::std::fmt::Display,
        {
            self.session_id = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for session_id: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<ResumeSessionRequest> for super::ResumeSessionRequest {
        type Error = super::error::ConversionError;
        fn try_from(
            value: ResumeSessionRequest,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                additional_directories: value.additional_directories?,
                cwd: value.cwd?,
                mcp_servers: value.mcp_servers?,
                meta: value.meta?,
                session_id: value.session_id?,
            })
        }
    }
    impl ::std::convert::From<super::ResumeSessionRequest> for ResumeSessionRequest {
        fn from(value: super::ResumeSessionRequest) -> Self {
            Self {
                additional_directories: Ok(value.additional_directories),
                cwd: Ok(value.cwd),
                mcp_servers: Ok(value.mcp_servers),
                meta: Ok(value.meta),
                session_id: Ok(value.session_id),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct ResumeSessionResponse {
        config_options: ::std::result::Result<
            ::std::option::Option<::std::vec::Vec<super::SessionConfigOption>>,
            ::std::string::String,
        >,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        modes: ::std::result::Result<
            ::std::option::Option<super::SessionModeState>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for ResumeSessionResponse {
        fn default() -> Self {
            Self {
                config_options: Ok(Default::default()),
                meta: Ok(Default::default()),
                modes: Ok(Default::default()),
            }
        }
    }
    impl ResumeSessionResponse {
        pub fn config_options<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<::std::vec::Vec<super::SessionConfigOption>>,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.config_options = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for config_options: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn modes<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::SessionModeState>>,
            T::Error: ::std::fmt::Display,
        {
            self.modes = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for modes: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<ResumeSessionResponse> for super::ResumeSessionResponse {
        type Error = super::error::ConversionError;
        fn try_from(
            value: ResumeSessionResponse,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                config_options: value.config_options?,
                meta: value.meta?,
                modes: value.modes?,
            })
        }
    }
    impl ::std::convert::From<super::ResumeSessionResponse> for ResumeSessionResponse {
        fn from(value: super::ResumeSessionResponse) -> Self {
            Self {
                config_options: Ok(value.config_options),
                meta: Ok(value.meta),
                modes: Ok(value.modes),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct SelectedPermissionOutcome {
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        option_id: ::std::result::Result<super::PermissionOptionId, ::std::string::String>,
    }
    impl ::std::default::Default for SelectedPermissionOutcome {
        fn default() -> Self {
            Self {
                meta: Ok(Default::default()),
                option_id: Err("no value supplied for option_id".to_string()),
            }
        }
    }
    impl SelectedPermissionOutcome {
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn option_id<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::PermissionOptionId>,
            T::Error: ::std::fmt::Display,
        {
            self.option_id = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for option_id: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<SelectedPermissionOutcome> for super::SelectedPermissionOutcome {
        type Error = super::error::ConversionError;
        fn try_from(
            value: SelectedPermissionOutcome,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                meta: value.meta?,
                option_id: value.option_id?,
            })
        }
    }
    impl ::std::convert::From<super::SelectedPermissionOutcome> for SelectedPermissionOutcome {
        fn from(value: super::SelectedPermissionOutcome) -> Self {
            Self {
                meta: Ok(value.meta),
                option_id: Ok(value.option_id),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct SessionAdditionalDirectoriesCapabilities {
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for SessionAdditionalDirectoriesCapabilities {
        fn default() -> Self {
            Self {
                meta: Ok(Default::default()),
            }
        }
    }
    impl SessionAdditionalDirectoriesCapabilities {
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<SessionAdditionalDirectoriesCapabilities>
        for super::SessionAdditionalDirectoriesCapabilities
    {
        type Error = super::error::ConversionError;
        fn try_from(
            value: SessionAdditionalDirectoriesCapabilities,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self { meta: value.meta? })
        }
    }
    impl ::std::convert::From<super::SessionAdditionalDirectoriesCapabilities>
        for SessionAdditionalDirectoriesCapabilities
    {
        fn from(value: super::SessionAdditionalDirectoriesCapabilities) -> Self {
            Self {
                meta: Ok(value.meta),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct SessionCapabilities {
        additional_directories: ::std::result::Result<
            ::std::option::Option<super::SessionAdditionalDirectoriesCapabilities>,
            ::std::string::String,
        >,
        close: ::std::result::Result<
            ::std::option::Option<super::SessionCloseCapabilities>,
            ::std::string::String,
        >,
        delete: ::std::result::Result<
            ::std::option::Option<super::SessionDeleteCapabilities>,
            ::std::string::String,
        >,
        list: ::std::result::Result<
            ::std::option::Option<super::SessionListCapabilities>,
            ::std::string::String,
        >,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        resume: ::std::result::Result<
            ::std::option::Option<super::SessionResumeCapabilities>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for SessionCapabilities {
        fn default() -> Self {
            Self {
                additional_directories: Ok(Default::default()),
                close: Ok(Default::default()),
                delete: Ok(Default::default()),
                list: Ok(Default::default()),
                meta: Ok(Default::default()),
                resume: Ok(Default::default()),
            }
        }
    }
    impl SessionCapabilities {
        pub fn additional_directories<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<super::SessionAdditionalDirectoriesCapabilities>,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.additional_directories = value.try_into().map_err(|e| {
                format!("error converting supplied value for additional_directories: {e}")
            });
            self
        }
        pub fn close<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::SessionCloseCapabilities>>,
            T::Error: ::std::fmt::Display,
        {
            self.close = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for close: {e}"));
            self
        }
        pub fn delete<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::SessionDeleteCapabilities>>,
            T::Error: ::std::fmt::Display,
        {
            self.delete = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for delete: {e}"));
            self
        }
        pub fn list<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::SessionListCapabilities>>,
            T::Error: ::std::fmt::Display,
        {
            self.list = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for list: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn resume<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::SessionResumeCapabilities>>,
            T::Error: ::std::fmt::Display,
        {
            self.resume = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for resume: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<SessionCapabilities> for super::SessionCapabilities {
        type Error = super::error::ConversionError;
        fn try_from(
            value: SessionCapabilities,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                additional_directories: value.additional_directories?,
                close: value.close?,
                delete: value.delete?,
                list: value.list?,
                meta: value.meta?,
                resume: value.resume?,
            })
        }
    }
    impl ::std::convert::From<super::SessionCapabilities> for SessionCapabilities {
        fn from(value: super::SessionCapabilities) -> Self {
            Self {
                additional_directories: Ok(value.additional_directories),
                close: Ok(value.close),
                delete: Ok(value.delete),
                list: Ok(value.list),
                meta: Ok(value.meta),
                resume: Ok(value.resume),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct SessionCloseCapabilities {
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for SessionCloseCapabilities {
        fn default() -> Self {
            Self {
                meta: Ok(Default::default()),
            }
        }
    }
    impl SessionCloseCapabilities {
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<SessionCloseCapabilities> for super::SessionCloseCapabilities {
        type Error = super::error::ConversionError;
        fn try_from(
            value: SessionCloseCapabilities,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self { meta: value.meta? })
        }
    }
    impl ::std::convert::From<super::SessionCloseCapabilities> for SessionCloseCapabilities {
        fn from(value: super::SessionCloseCapabilities) -> Self {
            Self {
                meta: Ok(value.meta),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct SessionConfigBoolean {
        current_value: ::std::result::Result<bool, ::std::string::String>,
    }
    impl ::std::default::Default for SessionConfigBoolean {
        fn default() -> Self {
            Self {
                current_value: Err("no value supplied for current_value".to_string()),
            }
        }
    }
    impl SessionConfigBoolean {
        pub fn current_value<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<bool>,
            T::Error: ::std::fmt::Display,
        {
            self.current_value = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for current_value: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<SessionConfigBoolean> for super::SessionConfigBoolean {
        type Error = super::error::ConversionError;
        fn try_from(
            value: SessionConfigBoolean,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                current_value: value.current_value?,
            })
        }
    }
    impl ::std::convert::From<super::SessionConfigBoolean> for SessionConfigBoolean {
        fn from(value: super::SessionConfigBoolean) -> Self {
            Self {
                current_value: Ok(value.current_value),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct SessionConfigOptionCategory {
        subtype_0: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
        subtype_1: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
        subtype_2: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
        subtype_3: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
        subtype_4: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for SessionConfigOptionCategory {
        fn default() -> Self {
            Self {
                subtype_0: Ok(Default::default()),
                subtype_1: Ok(Default::default()),
                subtype_2: Ok(Default::default()),
                subtype_3: Ok(Default::default()),
                subtype_4: Ok(Default::default()),
            }
        }
    }
    impl SessionConfigOptionCategory {
        pub fn subtype_0<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_0 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_0: {e}"));
            self
        }
        pub fn subtype_1<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_1 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_1: {e}"));
            self
        }
        pub fn subtype_2<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_2 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_2: {e}"));
            self
        }
        pub fn subtype_3<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_3 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_3: {e}"));
            self
        }
        pub fn subtype_4<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_4 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_4: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<SessionConfigOptionCategory> for super::SessionConfigOptionCategory {
        type Error = super::error::ConversionError;
        fn try_from(
            value: SessionConfigOptionCategory,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                subtype_0: value.subtype_0?,
                subtype_1: value.subtype_1?,
                subtype_2: value.subtype_2?,
                subtype_3: value.subtype_3?,
                subtype_4: value.subtype_4?,
            })
        }
    }
    impl ::std::convert::From<super::SessionConfigOptionCategory> for SessionConfigOptionCategory {
        fn from(value: super::SessionConfigOptionCategory) -> Self {
            Self {
                subtype_0: Ok(value.subtype_0),
                subtype_1: Ok(value.subtype_1),
                subtype_2: Ok(value.subtype_2),
                subtype_3: Ok(value.subtype_3),
                subtype_4: Ok(value.subtype_4),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct SessionConfigOptionsCapabilities {
        boolean: ::std::result::Result<
            ::std::option::Option<super::BooleanConfigOptionCapabilities>,
            ::std::string::String,
        >,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for SessionConfigOptionsCapabilities {
        fn default() -> Self {
            Self {
                boolean: Ok(Default::default()),
                meta: Ok(Default::default()),
            }
        }
    }
    impl SessionConfigOptionsCapabilities {
        pub fn boolean<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<super::BooleanConfigOptionCapabilities>,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.boolean = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for boolean: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<SessionConfigOptionsCapabilities>
        for super::SessionConfigOptionsCapabilities
    {
        type Error = super::error::ConversionError;
        fn try_from(
            value: SessionConfigOptionsCapabilities,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                boolean: value.boolean?,
                meta: value.meta?,
            })
        }
    }
    impl ::std::convert::From<super::SessionConfigOptionsCapabilities>
        for SessionConfigOptionsCapabilities
    {
        fn from(value: super::SessionConfigOptionsCapabilities) -> Self {
            Self {
                boolean: Ok(value.boolean),
                meta: Ok(value.meta),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct SessionConfigSelect {
        current_value: ::std::result::Result<super::SessionConfigValueId, ::std::string::String>,
        options: ::std::result::Result<super::SessionConfigSelectOptions, ::std::string::String>,
    }
    impl ::std::default::Default for SessionConfigSelect {
        fn default() -> Self {
            Self {
                current_value: Err("no value supplied for current_value".to_string()),
                options: Err("no value supplied for options".to_string()),
            }
        }
    }
    impl SessionConfigSelect {
        pub fn current_value<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::SessionConfigValueId>,
            T::Error: ::std::fmt::Display,
        {
            self.current_value = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for current_value: {e}"));
            self
        }
        pub fn options<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::SessionConfigSelectOptions>,
            T::Error: ::std::fmt::Display,
        {
            self.options = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for options: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<SessionConfigSelect> for super::SessionConfigSelect {
        type Error = super::error::ConversionError;
        fn try_from(
            value: SessionConfigSelect,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                current_value: value.current_value?,
                options: value.options?,
            })
        }
    }
    impl ::std::convert::From<super::SessionConfigSelect> for SessionConfigSelect {
        fn from(value: super::SessionConfigSelect) -> Self {
            Self {
                current_value: Ok(value.current_value),
                options: Ok(value.options),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct SessionConfigSelectGroup {
        group: ::std::result::Result<super::SessionConfigGroupId, ::std::string::String>,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        name: ::std::result::Result<::std::string::String, ::std::string::String>,
        options: ::std::result::Result<
            ::std::vec::Vec<super::SessionConfigSelectOption>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for SessionConfigSelectGroup {
        fn default() -> Self {
            Self {
                group: Err("no value supplied for group".to_string()),
                meta: Ok(Default::default()),
                name: Err("no value supplied for name".to_string()),
                options: Err("no value supplied for options".to_string()),
            }
        }
    }
    impl SessionConfigSelectGroup {
        pub fn group<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::SessionConfigGroupId>,
            T::Error: ::std::fmt::Display,
        {
            self.group = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for group: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn name<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.name = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for name: {e}"));
            self
        }
        pub fn options<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::vec::Vec<super::SessionConfigSelectOption>>,
            T::Error: ::std::fmt::Display,
        {
            self.options = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for options: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<SessionConfigSelectGroup> for super::SessionConfigSelectGroup {
        type Error = super::error::ConversionError;
        fn try_from(
            value: SessionConfigSelectGroup,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                group: value.group?,
                meta: value.meta?,
                name: value.name?,
                options: value.options?,
            })
        }
    }
    impl ::std::convert::From<super::SessionConfigSelectGroup> for SessionConfigSelectGroup {
        fn from(value: super::SessionConfigSelectGroup) -> Self {
            Self {
                group: Ok(value.group),
                meta: Ok(value.meta),
                name: Ok(value.name),
                options: Ok(value.options),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct SessionConfigSelectOption {
        description: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        name: ::std::result::Result<::std::string::String, ::std::string::String>,
        value: ::std::result::Result<super::SessionConfigValueId, ::std::string::String>,
    }
    impl ::std::default::Default for SessionConfigSelectOption {
        fn default() -> Self {
            Self {
                description: Ok(Default::default()),
                meta: Ok(Default::default()),
                name: Err("no value supplied for name".to_string()),
                value: Err("no value supplied for value".to_string()),
            }
        }
    }
    impl SessionConfigSelectOption {
        pub fn description<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.description = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for description: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn name<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.name = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for name: {e}"));
            self
        }
        pub fn value<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::SessionConfigValueId>,
            T::Error: ::std::fmt::Display,
        {
            self.value = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for value: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<SessionConfigSelectOption> for super::SessionConfigSelectOption {
        type Error = super::error::ConversionError;
        fn try_from(
            value: SessionConfigSelectOption,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                description: value.description?,
                meta: value.meta?,
                name: value.name?,
                value: value.value?,
            })
        }
    }
    impl ::std::convert::From<super::SessionConfigSelectOption> for SessionConfigSelectOption {
        fn from(value: super::SessionConfigSelectOption) -> Self {
            Self {
                description: Ok(value.description),
                meta: Ok(value.meta),
                name: Ok(value.name),
                value: Ok(value.value),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct SessionConfigSelectOptions {
        subtype_0: ::std::result::Result<
            ::std::option::Option<::std::vec::Vec<super::SessionConfigSelectOption>>,
            ::std::string::String,
        >,
        subtype_1: ::std::result::Result<
            ::std::option::Option<::std::vec::Vec<super::SessionConfigSelectGroup>>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for SessionConfigSelectOptions {
        fn default() -> Self {
            Self {
                subtype_0: Ok(Default::default()),
                subtype_1: Ok(Default::default()),
            }
        }
    }
    impl SessionConfigSelectOptions {
        pub fn subtype_0<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<::std::vec::Vec<super::SessionConfigSelectOption>>,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_0 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_0: {e}"));
            self
        }
        pub fn subtype_1<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<::std::vec::Vec<super::SessionConfigSelectGroup>>,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_1 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_1: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<SessionConfigSelectOptions> for super::SessionConfigSelectOptions {
        type Error = super::error::ConversionError;
        fn try_from(
            value: SessionConfigSelectOptions,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                subtype_0: value.subtype_0?,
                subtype_1: value.subtype_1?,
            })
        }
    }
    impl ::std::convert::From<super::SessionConfigSelectOptions> for SessionConfigSelectOptions {
        fn from(value: super::SessionConfigSelectOptions) -> Self {
            Self {
                subtype_0: Ok(value.subtype_0),
                subtype_1: Ok(value.subtype_1),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct SessionDeleteCapabilities {
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for SessionDeleteCapabilities {
        fn default() -> Self {
            Self {
                meta: Ok(Default::default()),
            }
        }
    }
    impl SessionDeleteCapabilities {
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<SessionDeleteCapabilities> for super::SessionDeleteCapabilities {
        type Error = super::error::ConversionError;
        fn try_from(
            value: SessionDeleteCapabilities,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self { meta: value.meta? })
        }
    }
    impl ::std::convert::From<super::SessionDeleteCapabilities> for SessionDeleteCapabilities {
        fn from(value: super::SessionDeleteCapabilities) -> Self {
            Self {
                meta: Ok(value.meta),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct SessionInfo {
        additional_directories:
            ::std::result::Result<::std::vec::Vec<::std::string::String>, ::std::string::String>,
        cwd: ::std::result::Result<::std::string::String, ::std::string::String>,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        session_id: ::std::result::Result<super::SessionId, ::std::string::String>,
        title: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
        updated_at: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for SessionInfo {
        fn default() -> Self {
            Self {
                additional_directories: Ok(Default::default()),
                cwd: Err("no value supplied for cwd".to_string()),
                meta: Ok(Default::default()),
                session_id: Err("no value supplied for session_id".to_string()),
                title: Ok(Default::default()),
                updated_at: Ok(Default::default()),
            }
        }
    }
    impl SessionInfo {
        pub fn additional_directories<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::vec::Vec<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.additional_directories = value.try_into().map_err(|e| {
                format!("error converting supplied value for additional_directories: {e}")
            });
            self
        }
        pub fn cwd<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.cwd = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for cwd: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn session_id<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::SessionId>,
            T::Error: ::std::fmt::Display,
        {
            self.session_id = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for session_id: {e}"));
            self
        }
        pub fn title<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.title = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for title: {e}"));
            self
        }
        pub fn updated_at<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.updated_at = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for updated_at: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<SessionInfo> for super::SessionInfo {
        type Error = super::error::ConversionError;
        fn try_from(
            value: SessionInfo,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                additional_directories: value.additional_directories?,
                cwd: value.cwd?,
                meta: value.meta?,
                session_id: value.session_id?,
                title: value.title?,
                updated_at: value.updated_at?,
            })
        }
    }
    impl ::std::convert::From<super::SessionInfo> for SessionInfo {
        fn from(value: super::SessionInfo) -> Self {
            Self {
                additional_directories: Ok(value.additional_directories),
                cwd: Ok(value.cwd),
                meta: Ok(value.meta),
                session_id: Ok(value.session_id),
                title: Ok(value.title),
                updated_at: Ok(value.updated_at),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct SessionInfoUpdate {
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        title: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
        updated_at: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for SessionInfoUpdate {
        fn default() -> Self {
            Self {
                meta: Ok(Default::default()),
                title: Ok(Default::default()),
                updated_at: Ok(Default::default()),
            }
        }
    }
    impl SessionInfoUpdate {
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn title<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.title = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for title: {e}"));
            self
        }
        pub fn updated_at<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.updated_at = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for updated_at: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<SessionInfoUpdate> for super::SessionInfoUpdate {
        type Error = super::error::ConversionError;
        fn try_from(
            value: SessionInfoUpdate,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                meta: value.meta?,
                title: value.title?,
                updated_at: value.updated_at?,
            })
        }
    }
    impl ::std::convert::From<super::SessionInfoUpdate> for SessionInfoUpdate {
        fn from(value: super::SessionInfoUpdate) -> Self {
            Self {
                meta: Ok(value.meta),
                title: Ok(value.title),
                updated_at: Ok(value.updated_at),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct SessionListCapabilities {
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for SessionListCapabilities {
        fn default() -> Self {
            Self {
                meta: Ok(Default::default()),
            }
        }
    }
    impl SessionListCapabilities {
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<SessionListCapabilities> for super::SessionListCapabilities {
        type Error = super::error::ConversionError;
        fn try_from(
            value: SessionListCapabilities,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self { meta: value.meta? })
        }
    }
    impl ::std::convert::From<super::SessionListCapabilities> for SessionListCapabilities {
        fn from(value: super::SessionListCapabilities) -> Self {
            Self {
                meta: Ok(value.meta),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct SessionMode {
        description: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
        id: ::std::result::Result<super::SessionModeId, ::std::string::String>,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        name: ::std::result::Result<::std::string::String, ::std::string::String>,
    }
    impl ::std::default::Default for SessionMode {
        fn default() -> Self {
            Self {
                description: Ok(Default::default()),
                id: Err("no value supplied for id".to_string()),
                meta: Ok(Default::default()),
                name: Err("no value supplied for name".to_string()),
            }
        }
    }
    impl SessionMode {
        pub fn description<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.description = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for description: {e}"));
            self
        }
        pub fn id<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::SessionModeId>,
            T::Error: ::std::fmt::Display,
        {
            self.id = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for id: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn name<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.name = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for name: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<SessionMode> for super::SessionMode {
        type Error = super::error::ConversionError;
        fn try_from(
            value: SessionMode,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                description: value.description?,
                id: value.id?,
                meta: value.meta?,
                name: value.name?,
            })
        }
    }
    impl ::std::convert::From<super::SessionMode> for SessionMode {
        fn from(value: super::SessionMode) -> Self {
            Self {
                description: Ok(value.description),
                id: Ok(value.id),
                meta: Ok(value.meta),
                name: Ok(value.name),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct SessionModeState {
        available_modes:
            ::std::result::Result<::std::vec::Vec<super::SessionMode>, ::std::string::String>,
        current_mode_id: ::std::result::Result<super::SessionModeId, ::std::string::String>,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for SessionModeState {
        fn default() -> Self {
            Self {
                available_modes: Err("no value supplied for available_modes".to_string()),
                current_mode_id: Err("no value supplied for current_mode_id".to_string()),
                meta: Ok(Default::default()),
            }
        }
    }
    impl SessionModeState {
        pub fn available_modes<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::vec::Vec<super::SessionMode>>,
            T::Error: ::std::fmt::Display,
        {
            self.available_modes = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for available_modes: {e}"));
            self
        }
        pub fn current_mode_id<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::SessionModeId>,
            T::Error: ::std::fmt::Display,
        {
            self.current_mode_id = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for current_mode_id: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<SessionModeState> for super::SessionModeState {
        type Error = super::error::ConversionError;
        fn try_from(
            value: SessionModeState,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                available_modes: value.available_modes?,
                current_mode_id: value.current_mode_id?,
                meta: value.meta?,
            })
        }
    }
    impl ::std::convert::From<super::SessionModeState> for SessionModeState {
        fn from(value: super::SessionModeState) -> Self {
            Self {
                available_modes: Ok(value.available_modes),
                current_mode_id: Ok(value.current_mode_id),
                meta: Ok(value.meta),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct SessionNotification {
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        session_id: ::std::result::Result<super::SessionId, ::std::string::String>,
        update: ::std::result::Result<super::SessionUpdate, ::std::string::String>,
    }
    impl ::std::default::Default for SessionNotification {
        fn default() -> Self {
            Self {
                meta: Ok(Default::default()),
                session_id: Err("no value supplied for session_id".to_string()),
                update: Err("no value supplied for update".to_string()),
            }
        }
    }
    impl SessionNotification {
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn session_id<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::SessionId>,
            T::Error: ::std::fmt::Display,
        {
            self.session_id = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for session_id: {e}"));
            self
        }
        pub fn update<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::SessionUpdate>,
            T::Error: ::std::fmt::Display,
        {
            self.update = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for update: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<SessionNotification> for super::SessionNotification {
        type Error = super::error::ConversionError;
        fn try_from(
            value: SessionNotification,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                meta: value.meta?,
                session_id: value.session_id?,
                update: value.update?,
            })
        }
    }
    impl ::std::convert::From<super::SessionNotification> for SessionNotification {
        fn from(value: super::SessionNotification) -> Self {
            Self {
                meta: Ok(value.meta),
                session_id: Ok(value.session_id),
                update: Ok(value.update),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct SessionResumeCapabilities {
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for SessionResumeCapabilities {
        fn default() -> Self {
            Self {
                meta: Ok(Default::default()),
            }
        }
    }
    impl SessionResumeCapabilities {
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<SessionResumeCapabilities> for super::SessionResumeCapabilities {
        type Error = super::error::ConversionError;
        fn try_from(
            value: SessionResumeCapabilities,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self { meta: value.meta? })
        }
    }
    impl ::std::convert::From<super::SessionResumeCapabilities> for SessionResumeCapabilities {
        fn from(value: super::SessionResumeCapabilities) -> Self {
            Self {
                meta: Ok(value.meta),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct SetSessionConfigOptionResponse {
        config_options: ::std::result::Result<
            ::std::vec::Vec<super::SessionConfigOption>,
            ::std::string::String,
        >,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for SetSessionConfigOptionResponse {
        fn default() -> Self {
            Self {
                config_options: Err("no value supplied for config_options".to_string()),
                meta: Ok(Default::default()),
            }
        }
    }
    impl SetSessionConfigOptionResponse {
        pub fn config_options<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::vec::Vec<super::SessionConfigOption>>,
            T::Error: ::std::fmt::Display,
        {
            self.config_options = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for config_options: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<SetSessionConfigOptionResponse>
        for super::SetSessionConfigOptionResponse
    {
        type Error = super::error::ConversionError;
        fn try_from(
            value: SetSessionConfigOptionResponse,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                config_options: value.config_options?,
                meta: value.meta?,
            })
        }
    }
    impl ::std::convert::From<super::SetSessionConfigOptionResponse>
        for SetSessionConfigOptionResponse
    {
        fn from(value: super::SetSessionConfigOptionResponse) -> Self {
            Self {
                config_options: Ok(value.config_options),
                meta: Ok(value.meta),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct SetSessionModeRequest {
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        mode_id: ::std::result::Result<super::SessionModeId, ::std::string::String>,
        session_id: ::std::result::Result<super::SessionId, ::std::string::String>,
    }
    impl ::std::default::Default for SetSessionModeRequest {
        fn default() -> Self {
            Self {
                meta: Ok(Default::default()),
                mode_id: Err("no value supplied for mode_id".to_string()),
                session_id: Err("no value supplied for session_id".to_string()),
            }
        }
    }
    impl SetSessionModeRequest {
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn mode_id<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::SessionModeId>,
            T::Error: ::std::fmt::Display,
        {
            self.mode_id = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for mode_id: {e}"));
            self
        }
        pub fn session_id<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::SessionId>,
            T::Error: ::std::fmt::Display,
        {
            self.session_id = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for session_id: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<SetSessionModeRequest> for super::SetSessionModeRequest {
        type Error = super::error::ConversionError;
        fn try_from(
            value: SetSessionModeRequest,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                meta: value.meta?,
                mode_id: value.mode_id?,
                session_id: value.session_id?,
            })
        }
    }
    impl ::std::convert::From<super::SetSessionModeRequest> for SetSessionModeRequest {
        fn from(value: super::SetSessionModeRequest) -> Self {
            Self {
                meta: Ok(value.meta),
                mode_id: Ok(value.mode_id),
                session_id: Ok(value.session_id),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct SetSessionModeResponse {
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for SetSessionModeResponse {
        fn default() -> Self {
            Self {
                meta: Ok(Default::default()),
            }
        }
    }
    impl SetSessionModeResponse {
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<SetSessionModeResponse> for super::SetSessionModeResponse {
        type Error = super::error::ConversionError;
        fn try_from(
            value: SetSessionModeResponse,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self { meta: value.meta? })
        }
    }
    impl ::std::convert::From<super::SetSessionModeResponse> for SetSessionModeResponse {
        fn from(value: super::SetSessionModeResponse) -> Self {
            Self {
                meta: Ok(value.meta),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct Terminal {
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        terminal_id: ::std::result::Result<super::TerminalId, ::std::string::String>,
    }
    impl ::std::default::Default for Terminal {
        fn default() -> Self {
            Self {
                meta: Ok(Default::default()),
                terminal_id: Err("no value supplied for terminal_id".to_string()),
            }
        }
    }
    impl Terminal {
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn terminal_id<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::TerminalId>,
            T::Error: ::std::fmt::Display,
        {
            self.terminal_id = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for terminal_id: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<Terminal> for super::Terminal {
        type Error = super::error::ConversionError;
        fn try_from(value: Terminal) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                meta: value.meta?,
                terminal_id: value.terminal_id?,
            })
        }
    }
    impl ::std::convert::From<super::Terminal> for Terminal {
        fn from(value: super::Terminal) -> Self {
            Self {
                meta: Ok(value.meta),
                terminal_id: Ok(value.terminal_id),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct TerminalExitStatus {
        exit_code: ::std::result::Result<::std::option::Option<u32>, ::std::string::String>,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        signal: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for TerminalExitStatus {
        fn default() -> Self {
            Self {
                exit_code: Ok(Default::default()),
                meta: Ok(Default::default()),
                signal: Ok(Default::default()),
            }
        }
    }
    impl TerminalExitStatus {
        pub fn exit_code<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<u32>>,
            T::Error: ::std::fmt::Display,
        {
            self.exit_code = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for exit_code: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn signal<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.signal = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for signal: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<TerminalExitStatus> for super::TerminalExitStatus {
        type Error = super::error::ConversionError;
        fn try_from(
            value: TerminalExitStatus,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                exit_code: value.exit_code?,
                meta: value.meta?,
                signal: value.signal?,
            })
        }
    }
    impl ::std::convert::From<super::TerminalExitStatus> for TerminalExitStatus {
        fn from(value: super::TerminalExitStatus) -> Self {
            Self {
                exit_code: Ok(value.exit_code),
                meta: Ok(value.meta),
                signal: Ok(value.signal),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct TerminalOutputRequest {
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        session_id: ::std::result::Result<super::SessionId, ::std::string::String>,
        terminal_id: ::std::result::Result<super::TerminalId, ::std::string::String>,
    }
    impl ::std::default::Default for TerminalOutputRequest {
        fn default() -> Self {
            Self {
                meta: Ok(Default::default()),
                session_id: Err("no value supplied for session_id".to_string()),
                terminal_id: Err("no value supplied for terminal_id".to_string()),
            }
        }
    }
    impl TerminalOutputRequest {
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn session_id<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::SessionId>,
            T::Error: ::std::fmt::Display,
        {
            self.session_id = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for session_id: {e}"));
            self
        }
        pub fn terminal_id<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::TerminalId>,
            T::Error: ::std::fmt::Display,
        {
            self.terminal_id = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for terminal_id: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<TerminalOutputRequest> for super::TerminalOutputRequest {
        type Error = super::error::ConversionError;
        fn try_from(
            value: TerminalOutputRequest,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                meta: value.meta?,
                session_id: value.session_id?,
                terminal_id: value.terminal_id?,
            })
        }
    }
    impl ::std::convert::From<super::TerminalOutputRequest> for TerminalOutputRequest {
        fn from(value: super::TerminalOutputRequest) -> Self {
            Self {
                meta: Ok(value.meta),
                session_id: Ok(value.session_id),
                terminal_id: Ok(value.terminal_id),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct TerminalOutputResponse {
        exit_status: ::std::result::Result<
            ::std::option::Option<super::TerminalExitStatus>,
            ::std::string::String,
        >,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        output: ::std::result::Result<::std::string::String, ::std::string::String>,
        truncated: ::std::result::Result<bool, ::std::string::String>,
    }
    impl ::std::default::Default for TerminalOutputResponse {
        fn default() -> Self {
            Self {
                exit_status: Ok(Default::default()),
                meta: Ok(Default::default()),
                output: Err("no value supplied for output".to_string()),
                truncated: Err("no value supplied for truncated".to_string()),
            }
        }
    }
    impl TerminalOutputResponse {
        pub fn exit_status<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::TerminalExitStatus>>,
            T::Error: ::std::fmt::Display,
        {
            self.exit_status = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for exit_status: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn output<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.output = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for output: {e}"));
            self
        }
        pub fn truncated<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<bool>,
            T::Error: ::std::fmt::Display,
        {
            self.truncated = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for truncated: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<TerminalOutputResponse> for super::TerminalOutputResponse {
        type Error = super::error::ConversionError;
        fn try_from(
            value: TerminalOutputResponse,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                exit_status: value.exit_status?,
                meta: value.meta?,
                output: value.output?,
                truncated: value.truncated?,
            })
        }
    }
    impl ::std::convert::From<super::TerminalOutputResponse> for TerminalOutputResponse {
        fn from(value: super::TerminalOutputResponse) -> Self {
            Self {
                exit_status: Ok(value.exit_status),
                meta: Ok(value.meta),
                output: Ok(value.output),
                truncated: Ok(value.truncated),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct TextContent {
        annotations:
            ::std::result::Result<::std::option::Option<super::Annotations>, ::std::string::String>,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        text: ::std::result::Result<::std::string::String, ::std::string::String>,
    }
    impl ::std::default::Default for TextContent {
        fn default() -> Self {
            Self {
                annotations: Ok(Default::default()),
                meta: Ok(Default::default()),
                text: Err("no value supplied for text".to_string()),
            }
        }
    }
    impl TextContent {
        pub fn annotations<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::Annotations>>,
            T::Error: ::std::fmt::Display,
        {
            self.annotations = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for annotations: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn text<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.text = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for text: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<TextContent> for super::TextContent {
        type Error = super::error::ConversionError;
        fn try_from(
            value: TextContent,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                annotations: value.annotations?,
                meta: value.meta?,
                text: value.text?,
            })
        }
    }
    impl ::std::convert::From<super::TextContent> for TextContent {
        fn from(value: super::TextContent) -> Self {
            Self {
                annotations: Ok(value.annotations),
                meta: Ok(value.meta),
                text: Ok(value.text),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct TextResourceContents {
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        mime_type: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
        text: ::std::result::Result<::std::string::String, ::std::string::String>,
        uri: ::std::result::Result<::std::string::String, ::std::string::String>,
    }
    impl ::std::default::Default for TextResourceContents {
        fn default() -> Self {
            Self {
                meta: Ok(Default::default()),
                mime_type: Ok(Default::default()),
                text: Err("no value supplied for text".to_string()),
                uri: Err("no value supplied for uri".to_string()),
            }
        }
    }
    impl TextResourceContents {
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn mime_type<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.mime_type = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for mime_type: {e}"));
            self
        }
        pub fn text<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.text = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for text: {e}"));
            self
        }
        pub fn uri<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.uri = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for uri: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<TextResourceContents> for super::TextResourceContents {
        type Error = super::error::ConversionError;
        fn try_from(
            value: TextResourceContents,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                meta: value.meta?,
                mime_type: value.mime_type?,
                text: value.text?,
                uri: value.uri?,
            })
        }
    }
    impl ::std::convert::From<super::TextResourceContents> for TextResourceContents {
        fn from(value: super::TextResourceContents) -> Self {
            Self {
                meta: Ok(value.meta),
                mime_type: Ok(value.mime_type),
                text: Ok(value.text),
                uri: Ok(value.uri),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct ToolCall {
        content:
            ::std::result::Result<::std::vec::Vec<super::ToolCallContent>, ::std::string::String>,
        kind: ::std::result::Result<::std::option::Option<super::ToolKind>, ::std::string::String>,
        locations:
            ::std::result::Result<::std::vec::Vec<super::ToolCallLocation>, ::std::string::String>,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        raw_input: ::std::result::Result<
            ::std::option::Option<::serde_json::Value>,
            ::std::string::String,
        >,
        raw_output: ::std::result::Result<
            ::std::option::Option<::serde_json::Value>,
            ::std::string::String,
        >,
        status: ::std::result::Result<
            ::std::option::Option<super::ToolCallStatus>,
            ::std::string::String,
        >,
        title: ::std::result::Result<::std::string::String, ::std::string::String>,
        tool_call_id: ::std::result::Result<super::ToolCallId, ::std::string::String>,
    }
    impl ::std::default::Default for ToolCall {
        fn default() -> Self {
            Self {
                content: Ok(Default::default()),
                kind: Ok(Default::default()),
                locations: Ok(Default::default()),
                meta: Ok(Default::default()),
                raw_input: Ok(Default::default()),
                raw_output: Ok(Default::default()),
                status: Ok(Default::default()),
                title: Err("no value supplied for title".to_string()),
                tool_call_id: Err("no value supplied for tool_call_id".to_string()),
            }
        }
    }
    impl ToolCall {
        pub fn content<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::vec::Vec<super::ToolCallContent>>,
            T::Error: ::std::fmt::Display,
        {
            self.content = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for content: {e}"));
            self
        }
        pub fn kind<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::ToolKind>>,
            T::Error: ::std::fmt::Display,
        {
            self.kind = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for kind: {e}"));
            self
        }
        pub fn locations<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::vec::Vec<super::ToolCallLocation>>,
            T::Error: ::std::fmt::Display,
        {
            self.locations = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for locations: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn raw_input<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::serde_json::Value>>,
            T::Error: ::std::fmt::Display,
        {
            self.raw_input = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for raw_input: {e}"));
            self
        }
        pub fn raw_output<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::serde_json::Value>>,
            T::Error: ::std::fmt::Display,
        {
            self.raw_output = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for raw_output: {e}"));
            self
        }
        pub fn status<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::ToolCallStatus>>,
            T::Error: ::std::fmt::Display,
        {
            self.status = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for status: {e}"));
            self
        }
        pub fn title<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.title = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for title: {e}"));
            self
        }
        pub fn tool_call_id<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::ToolCallId>,
            T::Error: ::std::fmt::Display,
        {
            self.tool_call_id = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for tool_call_id: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<ToolCall> for super::ToolCall {
        type Error = super::error::ConversionError;
        fn try_from(value: ToolCall) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                content: value.content?,
                kind: value.kind?,
                locations: value.locations?,
                meta: value.meta?,
                raw_input: value.raw_input?,
                raw_output: value.raw_output?,
                status: value.status?,
                title: value.title?,
                tool_call_id: value.tool_call_id?,
            })
        }
    }
    impl ::std::convert::From<super::ToolCall> for ToolCall {
        fn from(value: super::ToolCall) -> Self {
            Self {
                content: Ok(value.content),
                kind: Ok(value.kind),
                locations: Ok(value.locations),
                meta: Ok(value.meta),
                raw_input: Ok(value.raw_input),
                raw_output: Ok(value.raw_output),
                status: Ok(value.status),
                title: Ok(value.title),
                tool_call_id: Ok(value.tool_call_id),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct ToolCallLocation {
        line: ::std::result::Result<::std::option::Option<u32>, ::std::string::String>,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        path: ::std::result::Result<::std::string::String, ::std::string::String>,
    }
    impl ::std::default::Default for ToolCallLocation {
        fn default() -> Self {
            Self {
                line: Ok(Default::default()),
                meta: Ok(Default::default()),
                path: Err("no value supplied for path".to_string()),
            }
        }
    }
    impl ToolCallLocation {
        pub fn line<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<u32>>,
            T::Error: ::std::fmt::Display,
        {
            self.line = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for line: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn path<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.path = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for path: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<ToolCallLocation> for super::ToolCallLocation {
        type Error = super::error::ConversionError;
        fn try_from(
            value: ToolCallLocation,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                line: value.line?,
                meta: value.meta?,
                path: value.path?,
            })
        }
    }
    impl ::std::convert::From<super::ToolCallLocation> for ToolCallLocation {
        fn from(value: super::ToolCallLocation) -> Self {
            Self {
                line: Ok(value.line),
                meta: Ok(value.meta),
                path: Ok(value.path),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct ToolCallUpdate {
        content: ::std::result::Result<
            ::std::option::Option<::std::vec::Vec<super::ToolCallContent>>,
            ::std::string::String,
        >,
        kind: ::std::result::Result<::std::option::Option<super::ToolKind>, ::std::string::String>,
        locations: ::std::result::Result<
            ::std::option::Option<::std::vec::Vec<super::ToolCallLocation>>,
            ::std::string::String,
        >,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        raw_input: ::std::result::Result<
            ::std::option::Option<::serde_json::Value>,
            ::std::string::String,
        >,
        raw_output: ::std::result::Result<
            ::std::option::Option<::serde_json::Value>,
            ::std::string::String,
        >,
        status: ::std::result::Result<
            ::std::option::Option<super::ToolCallStatus>,
            ::std::string::String,
        >,
        title: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
        tool_call_id: ::std::result::Result<super::ToolCallId, ::std::string::String>,
    }
    impl ::std::default::Default for ToolCallUpdate {
        fn default() -> Self {
            Self {
                content: Ok(Default::default()),
                kind: Ok(Default::default()),
                locations: Ok(Default::default()),
                meta: Ok(Default::default()),
                raw_input: Ok(Default::default()),
                raw_output: Ok(Default::default()),
                status: Ok(Default::default()),
                title: Ok(Default::default()),
                tool_call_id: Err("no value supplied for tool_call_id".to_string()),
            }
        }
    }
    impl ToolCallUpdate {
        pub fn content<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<::std::vec::Vec<super::ToolCallContent>>,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.content = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for content: {e}"));
            self
        }
        pub fn kind<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::ToolKind>>,
            T::Error: ::std::fmt::Display,
        {
            self.kind = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for kind: {e}"));
            self
        }
        pub fn locations<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<::std::vec::Vec<super::ToolCallLocation>>,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.locations = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for locations: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn raw_input<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::serde_json::Value>>,
            T::Error: ::std::fmt::Display,
        {
            self.raw_input = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for raw_input: {e}"));
            self
        }
        pub fn raw_output<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::serde_json::Value>>,
            T::Error: ::std::fmt::Display,
        {
            self.raw_output = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for raw_output: {e}"));
            self
        }
        pub fn status<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::ToolCallStatus>>,
            T::Error: ::std::fmt::Display,
        {
            self.status = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for status: {e}"));
            self
        }
        pub fn title<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.title = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for title: {e}"));
            self
        }
        pub fn tool_call_id<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::ToolCallId>,
            T::Error: ::std::fmt::Display,
        {
            self.tool_call_id = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for tool_call_id: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<ToolCallUpdate> for super::ToolCallUpdate {
        type Error = super::error::ConversionError;
        fn try_from(
            value: ToolCallUpdate,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                content: value.content?,
                kind: value.kind?,
                locations: value.locations?,
                meta: value.meta?,
                raw_input: value.raw_input?,
                raw_output: value.raw_output?,
                status: value.status?,
                title: value.title?,
                tool_call_id: value.tool_call_id?,
            })
        }
    }
    impl ::std::convert::From<super::ToolCallUpdate> for ToolCallUpdate {
        fn from(value: super::ToolCallUpdate) -> Self {
            Self {
                content: Ok(value.content),
                kind: Ok(value.kind),
                locations: Ok(value.locations),
                meta: Ok(value.meta),
                raw_input: Ok(value.raw_input),
                raw_output: Ok(value.raw_output),
                status: Ok(value.status),
                title: Ok(value.title),
                tool_call_id: Ok(value.tool_call_id),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct UnstructuredCommandInput {
        hint: ::std::result::Result<::std::string::String, ::std::string::String>,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for UnstructuredCommandInput {
        fn default() -> Self {
            Self {
                hint: Err("no value supplied for hint".to_string()),
                meta: Ok(Default::default()),
            }
        }
    }
    impl UnstructuredCommandInput {
        pub fn hint<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.hint = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for hint: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<UnstructuredCommandInput> for super::UnstructuredCommandInput {
        type Error = super::error::ConversionError;
        fn try_from(
            value: UnstructuredCommandInput,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                hint: value.hint?,
                meta: value.meta?,
            })
        }
    }
    impl ::std::convert::From<super::UnstructuredCommandInput> for UnstructuredCommandInput {
        fn from(value: super::UnstructuredCommandInput) -> Self {
            Self {
                hint: Ok(value.hint),
                meta: Ok(value.meta),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct UsageUpdate {
        cost: ::std::result::Result<::std::option::Option<super::Cost>, ::std::string::String>,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        size: ::std::result::Result<u64, ::std::string::String>,
        used: ::std::result::Result<u64, ::std::string::String>,
    }
    impl ::std::default::Default for UsageUpdate {
        fn default() -> Self {
            Self {
                cost: Ok(Default::default()),
                meta: Ok(Default::default()),
                size: Err("no value supplied for size".to_string()),
                used: Err("no value supplied for used".to_string()),
            }
        }
    }
    impl UsageUpdate {
        pub fn cost<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::Cost>>,
            T::Error: ::std::fmt::Display,
        {
            self.cost = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for cost: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn size<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<u64>,
            T::Error: ::std::fmt::Display,
        {
            self.size = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for size: {e}"));
            self
        }
        pub fn used<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<u64>,
            T::Error: ::std::fmt::Display,
        {
            self.used = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for used: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<UsageUpdate> for super::UsageUpdate {
        type Error = super::error::ConversionError;
        fn try_from(
            value: UsageUpdate,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                cost: value.cost?,
                meta: value.meta?,
                size: value.size?,
                used: value.used?,
            })
        }
    }
    impl ::std::convert::From<super::UsageUpdate> for UsageUpdate {
        fn from(value: super::UsageUpdate) -> Self {
            Self {
                cost: Ok(value.cost),
                meta: Ok(value.meta),
                size: Ok(value.size),
                used: Ok(value.used),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct WaitForTerminalExitRequest {
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        session_id: ::std::result::Result<super::SessionId, ::std::string::String>,
        terminal_id: ::std::result::Result<super::TerminalId, ::std::string::String>,
    }
    impl ::std::default::Default for WaitForTerminalExitRequest {
        fn default() -> Self {
            Self {
                meta: Ok(Default::default()),
                session_id: Err("no value supplied for session_id".to_string()),
                terminal_id: Err("no value supplied for terminal_id".to_string()),
            }
        }
    }
    impl WaitForTerminalExitRequest {
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn session_id<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::SessionId>,
            T::Error: ::std::fmt::Display,
        {
            self.session_id = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for session_id: {e}"));
            self
        }
        pub fn terminal_id<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::TerminalId>,
            T::Error: ::std::fmt::Display,
        {
            self.terminal_id = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for terminal_id: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<WaitForTerminalExitRequest> for super::WaitForTerminalExitRequest {
        type Error = super::error::ConversionError;
        fn try_from(
            value: WaitForTerminalExitRequest,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                meta: value.meta?,
                session_id: value.session_id?,
                terminal_id: value.terminal_id?,
            })
        }
    }
    impl ::std::convert::From<super::WaitForTerminalExitRequest> for WaitForTerminalExitRequest {
        fn from(value: super::WaitForTerminalExitRequest) -> Self {
            Self {
                meta: Ok(value.meta),
                session_id: Ok(value.session_id),
                terminal_id: Ok(value.terminal_id),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct WaitForTerminalExitResponse {
        exit_code: ::std::result::Result<::std::option::Option<u32>, ::std::string::String>,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        signal: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for WaitForTerminalExitResponse {
        fn default() -> Self {
            Self {
                exit_code: Ok(Default::default()),
                meta: Ok(Default::default()),
                signal: Ok(Default::default()),
            }
        }
    }
    impl WaitForTerminalExitResponse {
        pub fn exit_code<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<u32>>,
            T::Error: ::std::fmt::Display,
        {
            self.exit_code = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for exit_code: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn signal<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.signal = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for signal: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<WaitForTerminalExitResponse> for super::WaitForTerminalExitResponse {
        type Error = super::error::ConversionError;
        fn try_from(
            value: WaitForTerminalExitResponse,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                exit_code: value.exit_code?,
                meta: value.meta?,
                signal: value.signal?,
            })
        }
    }
    impl ::std::convert::From<super::WaitForTerminalExitResponse> for WaitForTerminalExitResponse {
        fn from(value: super::WaitForTerminalExitResponse) -> Self {
            Self {
                exit_code: Ok(value.exit_code),
                meta: Ok(value.meta),
                signal: Ok(value.signal),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct WriteTextFileRequest {
        content: ::std::result::Result<::std::string::String, ::std::string::String>,
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
        path: ::std::result::Result<::std::string::String, ::std::string::String>,
        session_id: ::std::result::Result<super::SessionId, ::std::string::String>,
    }
    impl ::std::default::Default for WriteTextFileRequest {
        fn default() -> Self {
            Self {
                content: Err("no value supplied for content".to_string()),
                meta: Ok(Default::default()),
                path: Err("no value supplied for path".to_string()),
                session_id: Err("no value supplied for session_id".to_string()),
            }
        }
    }
    impl WriteTextFileRequest {
        pub fn content<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.content = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for content: {e}"));
            self
        }
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
        pub fn path<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.path = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for path: {e}"));
            self
        }
        pub fn session_id<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::SessionId>,
            T::Error: ::std::fmt::Display,
        {
            self.session_id = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for session_id: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<WriteTextFileRequest> for super::WriteTextFileRequest {
        type Error = super::error::ConversionError;
        fn try_from(
            value: WriteTextFileRequest,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                content: value.content?,
                meta: value.meta?,
                path: value.path?,
                session_id: value.session_id?,
            })
        }
    }
    impl ::std::convert::From<super::WriteTextFileRequest> for WriteTextFileRequest {
        fn from(value: super::WriteTextFileRequest) -> Self {
            Self {
                content: Ok(value.content),
                meta: Ok(value.meta),
                path: Ok(value.path),
                session_id: Ok(value.session_id),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct WriteTextFileResponse {
        meta: ::std::result::Result<
            ::std::option::Option<::serde_json::Map<::std::string::String, ::serde_json::Value>>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for WriteTextFileResponse {
        fn default() -> Self {
            Self {
                meta: Ok(Default::default()),
            }
        }
    }
    impl WriteTextFileResponse {
        pub fn meta<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::option::Option<
                    ::serde_json::Map<::std::string::String, ::serde_json::Value>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.meta = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for meta: {e}"));
            self
        }
    }
    impl ::std::convert::TryFrom<WriteTextFileResponse> for super::WriteTextFileResponse {
        type Error = super::error::ConversionError;
        fn try_from(
            value: WriteTextFileResponse,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self { meta: value.meta? })
        }
    }
    impl ::std::convert::From<super::WriteTextFileResponse> for WriteTextFileResponse {
        fn from(value: super::WriteTextFileResponse) -> Self {
            Self {
                meta: Ok(value.meta),
            }
        }
    }
}
#[doc = r" Generation of default values for serde."]
pub mod defaults {
    pub(super) fn agent_capabilities_auth() -> super::AgentAuthCapabilities {
        super::AgentAuthCapabilities {
            logout: Default::default(),
            meta: Default::default(),
        }
    }
    pub(super) fn agent_capabilities_mcp_capabilities() -> super::McpCapabilities {
        super::McpCapabilities {
            http: false,
            meta: Default::default(),
            sse: false,
        }
    }
    pub(super) fn agent_capabilities_prompt_capabilities() -> super::PromptCapabilities {
        super::PromptCapabilities {
            audio: false,
            embedded_context: false,
            image: false,
            meta: Default::default(),
        }
    }
    pub(super) fn agent_capabilities_session_capabilities() -> super::SessionCapabilities {
        super::SessionCapabilities {
            additional_directories: Default::default(),
            close: Default::default(),
            delete: Default::default(),
            list: Default::default(),
            meta: Default::default(),
            resume: Default::default(),
        }
    }
    pub(super) fn client_capabilities_fs() -> super::FileSystemCapabilities {
        super::FileSystemCapabilities {
            meta: Default::default(),
            read_text_file: false,
            write_text_file: false,
        }
    }
    pub(super) fn initialize_request_client_capabilities() -> super::ClientCapabilities {
        super::ClientCapabilities {
            fs: super::FileSystemCapabilities {
                meta: Default::default(),
                read_text_file: false,
                write_text_file: false,
            },
            meta: Default::default(),
            session: Default::default(),
            terminal: false,
        }
    }
    pub(super) fn initialize_response_agent_capabilities() -> super::AgentCapabilities {
        super::AgentCapabilities {
            auth: super::AgentAuthCapabilities {
                logout: Default::default(),
                meta: Default::default(),
            },
            load_session: false,
            mcp_capabilities: super::McpCapabilities {
                http: false,
                meta: Default::default(),
                sse: false,
            },
            meta: Default::default(),
            prompt_capabilities: super::PromptCapabilities {
                audio: false,
                embedded_context: false,
                image: false,
                meta: Default::default(),
            },
            session_capabilities: super::SessionCapabilities {
                additional_directories: Default::default(),
                close: Default::default(),
                delete: Default::default(),
                list: Default::default(),
                meta: Default::default(),
                resume: Default::default(),
            },
        }
    }
}
