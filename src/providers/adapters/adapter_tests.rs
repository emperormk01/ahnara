#[cfg(test)]
mod tests {
    use crate::providers::{
        adapters::{
            anthropic::AnthropicAdapter,
            gemini::GeminiAdapter,
            openai::OpenAIAdapter,
            ProviderAdapter,
        },
        CompletionRequest, FunctionCall, FunctionDefinition, Message, ToolCall, ToolDefinition,
    };

    fn make_request(
        messages: Vec<Message>,
        tools: Option<Vec<ToolDefinition>>,
    ) -> CompletionRequest {
        CompletionRequest {
            model: "test-model".into(),
            messages,
            temperature: Some(0.7),
            max_tokens: Some(4096),
            tools,
            stream: Some(false),
            base_url: None,
            api_key: None,
        }
    }

    // --- Anthropic adapter ---

    #[test]
    fn anthropic_builds_correct_url() {
        let a = AnthropicAdapter::new();
        let url = a.build_url("https://api.anthropic.com/v1", "sk-test", "claude-3");
        assert_eq!(url, "https://api.anthropic.com/v1/messages");
    }

    #[test]
    fn anthropic_has_x_api_key_header() {
        let a = AnthropicAdapter::new();
        let headers = a.build_headers("sk-test");
        assert_eq!(headers.get("x-api-key").unwrap(), "sk-test");
        assert_eq!(
            headers.get("anthropic-version").unwrap(),
            "2023-06-01"
        );
    }

    #[test]
    fn anthropic_system_to_top_level_field() {
        let a = AnthropicAdapter::new();
        let req = make_request(
            vec![
                Message::new("system", "You are a helper"),
                Message::new("user", "Hi"),
            ],
            None,
        );
        let body = a.transform_request(&req);
        let sys = body["system"].as_array().unwrap();
        assert_eq!(sys.len(), 1);
        assert_eq!(sys[0]["type"], "text");
        assert_eq!(sys[0]["text"], "You are a helper");
        assert_eq!(sys[0]["cache_control"]["type"], "ephemeral");
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"], "user");
    }

    #[test]
    fn anthropic_multi_system_messages_merged() {
        let a = AnthropicAdapter::new();
        let req = make_request(
            vec![
                Message::new("system", "Prompt part 1"),
                Message::new("system", "Prompt part 2"),
                Message::new("user", "Hello"),
            ],
            None,
        );
        let body = a.transform_request(&req);
        let sys = body["system"].as_array().unwrap();
        let text = sys[0]["text"].as_str().unwrap();
        assert!(text.contains("Prompt part 1"));
        assert!(text.contains("Prompt part 2"));
    }

    #[test]
    fn anthropic_tool_calls_to_content_blocks() {
        let a = AnthropicAdapter::new();
        let req = make_request(
            vec![
                Message::new("system", "sys"),
                Message {
                    role: "assistant".into(),
                    content: Some("Let me search".into()),
                    tool_calls: Some(vec![ToolCall {
                        id: "tlu_01".into(),
                        call_type: "function".into(),
                        function: FunctionCall {
                            name: "web_search".into(),
                            arguments: r#"{"query":"rust"}"#.into(),
                        },
                    }]),
                    tool_call_id: None,
                    name: None,
                    content_parts: None,
                },
                Message {
                    role: "tool".into(),
                    content: Some("Found results".into()),
                    tool_calls: None,
                    tool_call_id: Some("tlu_01".into()),
                    name: Some("web_search".into()),
                    content_parts: None,
                },
            ],
            None,
        );
        let body = a.transform_request(&req);
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 2);
        let assistant_content = msgs[0]["content"].as_array().unwrap();
        assert_eq!(assistant_content.len(), 2);
        assert_eq!(assistant_content[0]["type"], "text");
        assert_eq!(assistant_content[1]["type"], "tool_use");
        let tool_result = msgs[1]["content"].as_array().unwrap();
        assert_eq!(tool_result[0]["type"], "tool_result");
        assert_eq!(tool_result[0]["tool_use_id"], "tlu_01");
    }

    #[test]
    fn anthropic_tools_list_transformed() {
        let a = AnthropicAdapter::new();
        let tools = vec![ToolDefinition {
            tool_type: "function".into(),
            function: FunctionDefinition {
                name: "search".into(),
                description: "Search web".into(),
                parameters: serde_json::json!({"type": "object", "properties": {}}),
            },
        }];
        let req = make_request(
            vec![Message::new("system", "s"), Message::new("user", "q")],
            Some(tools),
        );
        let body = a.transform_request(&req);
        let at = body["tools"].as_array().unwrap();
        assert_eq!(at.len(), 1);
        assert_eq!(at[0]["name"], "search");
        assert_eq!(at[0]["input_schema"]["type"], "object");
    }

    #[test]
    fn anthropic_parses_text_response() {
        let a = AnthropicAdapter::new();
        let body = serde_json::json!({
            "content": [{"type": "text", "text": "Hello there"}],
            "usage": {"input_tokens": 10, "output_tokens": 5}
        });
        let r = a.parse_response(&body.to_string()).unwrap();
        assert_eq!(r.content, "Hello there");
        assert!(r.tool_calls.is_none());
        assert_eq!(r.usage.unwrap().prompt_tokens, 10);
    }

    #[test]
    fn anthropic_parses_tool_use() {
        let a = AnthropicAdapter::new();
        let body = serde_json::json!({
            "content": [
                {"type": "text", "text": "Searching"},
                {"type": "tool_use", "id": "tlu_01", "name": "search", "input": {"q": "rust"}}
            ]
        });
        let r = a.parse_response(&body.to_string()).unwrap();
        assert!(r.content.contains("Searching"));
        let tc = r.tool_calls.unwrap();
        assert_eq!(tc[0].id, "tlu_01");
        assert_eq!(tc[0].function.name, "search");
    }

    // --- Gemini adapter ---

    #[test]
    fn gemini_url_has_key_param() {
        let a = GeminiAdapter::new();
        let url = a.build_url(
            "https://generativelanguage.googleapis.com/v1beta/openai",
            "mykey",
            "gemini",
        );
        assert!(url.contains("?key=mykey"));
        assert!(url.contains("/chat/completions"));
    }

    #[test]
    fn gemini_strips_model_prefix() {
        let a = GeminiAdapter::new();
        let mut req = make_request(
            vec![Message::new("system", "s"), Message::new("user", "h")],
            None,
        );
        req.model = "google/gemini-2.5-flash".into();
        let body = a.transform_request(&req);
        assert_eq!(body["model"], "gemini-2.5-flash");
    }

    // --- OpenAI adapter ---

    #[test]
    fn openai_builds_standard_url() {
        let a = OpenAIAdapter::new();
        let url = a.build_url("https://api.openai.com/v1", "sk-", "gpt-4");
        assert_eq!(url, "https://api.openai.com/v1/chat/completions");
    }

    #[test]
    fn openai_parses_tool_calls() {
        let a = OpenAIAdapter::new();
        let body = serde_json::json!({
            "choices": [{
                "message": {
                    "content": "ok",
                    "tool_calls": [{
                        "id": "c1", "type": "function",
                        "function": {"name": "s", "arguments": "{}"}
                    }]
                }
            }]
        });
        let r = a.parse_response(&body.to_string()).unwrap();
        assert_eq!(r.content, "ok");
        assert_eq!(r.tool_calls.unwrap()[0].function.name, "s");
    }

    #[test]
    fn openai_parses_reasoning_fallback() {
        let a = OpenAIAdapter::new();
        let body = serde_json::json!({
            "choices": [{"message": {"reasoning": "think", "content": null}}]
        });
        let r = a.parse_response(&body.to_string()).unwrap();
        assert_eq!(r.content, "think");
    }

    #[test]
    fn anthropic_tools_carry_cache_breakpoint() {
        let a = AnthropicAdapter::new();
        let tools = vec![
            ToolDefinition {
                tool_type: "function".into(),
                function: FunctionDefinition {
                    name: "one".into(),
                    description: "First".into(),
                    parameters: serde_json::json!({"type": "object"}),
                },
            },
            ToolDefinition {
                tool_type: "function".into(),
                function: FunctionDefinition {
                    name: "two".into(),
                    description: "Second".into(),
                    parameters: serde_json::json!({"type": "object"}),
                },
            },
        ];
        let req = make_request(
            vec![Message::new("system", "s"), Message::new("user", "q")],
            Some(tools),
        );
        let body = a.transform_request(&req);
        let at = body["tools"].as_array().unwrap();
        assert_eq!(at.len(), 2);
        assert!(at[0].get("cache_control").is_none());
        assert_eq!(at[1]["cache_control"]["type"], "ephemeral");
    }
}
