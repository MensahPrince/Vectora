//! Hand-run provider probe: one real completion through the configured
//! endpoint, tools attached. Not a test — needs a live endpoint.
//!
//! ```bash
//! # ~/.cutlass/config.toml:  [ai] base_url/model (see config.rs docs)
//! cargo run -p cutlass-ai --example chat_probe -- "what tools do you have?"
//! ```

use std::sync::atomic::AtomicBool;

use cutlass_ai::config::{default_config_path, load_ai_config};
use cutlass_ai::provider::{ChatProvider, ChatRequest, Message};
use cutlass_ai::providers::OpenAiCompatProvider;

fn main() {
    let prompt = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "Reply with one sentence: what kind of assistant are you?".to_string());

    let path = default_config_path();
    let section = match load_ai_config(&path) {
        Ok(Some(section)) => section,
        Ok(None) => {
            eprintln!(
                "no [ai] section in {}; see cutlass-ai/src/config.rs",
                path.display()
            );
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    println!("endpoint: {}  model: {}\n", section.base_url, section.model);

    let provider = OpenAiCompatProvider::from_config(&section).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(1);
    });

    let messages = vec![
        Message::System {
            content: "You are the editing agent inside the Cutlass video editor. \
                      You edit the timeline by calling tools."
                .to_string(),
        },
        Message::User { content: prompt },
    ];
    let mut tools = cutlass_ai::tool_specs();
    tools.push(cutlass_ai::wire::describe_project_spec());

    let cancel = AtomicBool::new(false);
    let turn = provider
        .chat(
            &ChatRequest {
                messages: &messages,
                tools: &tools,
            },
            &cancel,
            &mut |delta| {
                print!("{delta}");
                use std::io::Write;
                std::io::stdout().flush().ok();
            },
        )
        .unwrap_or_else(|e| {
            eprintln!("\nprovider error: {e}");
            std::process::exit(1);
        });

    println!("\n\nfinish: {:?}", turn.finish);
    for call in &turn.tool_calls {
        println!("tool call: {}({})", call.name, call.arguments);
    }
}
