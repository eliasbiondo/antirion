//! The conversation as a block-based target (Anthropic, Gemini, Converse) receives it: system
//! messages after the first turn become user text, same-role turns merge, empty text and empty
//! turns go, and a conversation never starts with the model.

use super::Enc;
use crate::xlate::ir::{Block, Role};
use crate::xlate::target::Upstream;
use crate::xlate::warn::{Kind, Src};

/// A normalised turn: `User` (which includes tool results) or `Assistant`.
#[derive(Clone, Debug)]
pub(super) struct NTurn {
    pub(super) role: Role,
    pub(super) blocks: Vec<Block>,
}

/// Sets a message-level marker on the message's last block that can carry one.
fn attach_cache(blocks: &mut [Block], cache: &crate::xlate::ir::Cache) {
    for b in blocks.iter_mut().rev() {
        let slot = match b {
            Block::Text { cache, .. } | Block::Image { cache, .. } | Block::Document { cache, .. } => cache,
            Block::ToolCall(c) => &mut c.cache,
            Block::ToolResult(r) => &mut r.cache,
            _ => continue,
        };
        if slot.is_none() {
            *slot = Some(cache.clone());
        }
        return;
    }
}

impl Enc<'_> {
    /// The turns of the request with history reasoning filtered (every target), message-level
    /// markers moved to their last block.
    pub(super) fn plain_turns(&mut self) -> Vec<(Role, Vec<Block>, Src)> {
        let req = self.req;
        let mut out = Vec::new();
        for t in &req.turns {
            let mut blocks: Vec<Block> = Vec::new();
            for b in &t.blocks {
                if self.keep_block(b) {
                    blocks.push(b.clone());
                }
            }
            if let Some(c) = &t.cache {
                attach_cache(&mut blocks, c);
            }
            out.push((t.role, blocks, t.src.clone()));
        }
        out
    }

    /// The turns for Anthropic, Gemini and Converse.
    pub(super) fn block_turns(&mut self) -> Vec<NTurn> {
        let req = self.req;
        let mut out: Vec<NTurn> = Vec::new();
        let plain = self.plain_turns();
        for ((role, mut blocks, _), t) in plain.into_iter().zip(&req.turns) {
            let role = match role {
                Role::System | Role::Developer => {
                    self.warn(Kind::Approximated, &t.role_src);
                    let text: Vec<&str> = blocks
                        .iter()
                        .filter_map(|b| match b {
                            Block::Text { text, .. } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect();
                    blocks = vec![Block::Text {
                        text: format!("<system>\n{}\n</system>", text.join("\n")),
                        cache: None,
                        src: t.src.clone(),
                    }];
                    Role::User
                }
                Role::Tool => Role::User,
                r => r,
            };
            blocks.retain(|b| !matches!(b, Block::Text { text, .. } if text.is_empty()));
            if blocks.is_empty() {
                continue;
            }
            match out.last_mut() {
                Some(last) if last.role == role => last.blocks.extend(blocks),
                _ => out.push(NTurn { role, blocks }),
            }
        }
        if matches!(self.dialect(), Upstream::Gemini | Upstream::Converse)
            && out.first().is_some_and(|t| t.role == Role::Assistant)
        {
            let src = req.messages_src.clone();
            self.warn(Kind::Approximated, &src);
            out.insert(
                0,
                NTurn {
                    role: Role::User,
                    blocks: vec![Block::Text {
                        text: ".".into(),
                        cache: None,
                        src,
                    }],
                },
            );
        }
        out
    }

    /// Whether the last assistant turn has a tool call but no reasoning block this target replays.
    pub(super) fn unsigned_tool_loop(&self) -> bool {
        let Some(last) = self.req.turns.iter().rev().find(|t| t.role == Role::Assistant) else {
            return false;
        };
        let has_call = last.blocks.iter().any(|b| matches!(b, Block::ToolCall(_)));
        let signed = last
            .blocks
            .iter()
            .any(|b| matches!(b, Block::Reasoning(r) if self.replayable(r) && r.signature.is_some()));
        has_call && !signed
    }
}
