//! Command pattern: every editor mutation is a [`Command`] with `apply` and
//! `revert`, so undo/redo is uniform across the whole editor and future
//! collaborative/sync tools. The engine provides the stack; the editor
//! defines its commands against [`CommandCtx`].

use crate::ecs::Registry;
use hecs::World;

/// What a command is allowed to touch.
pub struct CommandCtx<'a> {
    pub world: &'a mut World,
    pub registry: &'a Registry,
}

/// A reversible editor mutation.
pub trait Command: Send {
    /// Short description for the editor's undo history UI.
    fn label(&self) -> String;
    fn apply(&mut self, ctx: &mut CommandCtx);
    fn revert(&mut self, ctx: &mut CommandCtx);
}

/// Undo/redo stacks with a bounded history.
pub struct CommandStack {
    undo: Vec<Box<dyn Command>>,
    redo: Vec<Box<dyn Command>>,
    limit: usize,
}

impl Default for CommandStack {
    fn default() -> Self {
        Self { undo: Vec::new(), redo: Vec::new(), limit: 256 }
    }
}

impl CommandStack {
    /// Apply `cmd` and push it onto the undo stack (clears redo).
    pub fn push(&mut self, ctx: &mut CommandCtx, mut cmd: Box<dyn Command>) {
        cmd.apply(ctx);
        self.undo.push(cmd);
        self.redo.clear();
        if self.undo.len() > self.limit {
            self.undo.remove(0);
        }
    }

    pub fn undo(&mut self, ctx: &mut CommandCtx) -> Option<String> {
        let mut cmd = self.undo.pop()?;
        let label = cmd.label();
        cmd.revert(ctx);
        self.redo.push(cmd);
        Some(label)
    }

    pub fn redo(&mut self, ctx: &mut CommandCtx) -> Option<String> {
        let mut cmd = self.redo.pop()?;
        let label = cmd.label();
        cmd.apply(ctx);
        self.undo.push(cmd);
        Some(label)
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }

    /// Most recent undo label (for the edit menu).
    pub fn peek_undo(&self) -> Option<String> {
        self.undo.last().map(|c| c.label())
    }

    /// Most recent redo label (for the edit menu).
    pub fn peek_redo(&self) -> Option<String> {
        self.redo.last().map(|c| c.label())
    }
}
