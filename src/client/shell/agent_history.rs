use super::*;

/// The number of agent selections one history keeps.
const AGENT_HISTORY_LIMIT: usize = 50;

/// The agent histories of every machine as the save key stored them.
#[derive(Debug)]
pub(super) struct SavedAgentHistory {
    /// The machine that was active, which restore returns to.
    active_endpoint_id: ClientEndpointId,
    histories: HashMap<ClientEndpointId, AgentHistory>,
}

impl ClientShellState {
    pub(super) fn save_agent_history(&mut self, outcome: &mut ClientShellInput) {
        self.saved_agent_history = Some(SavedAgentHistory {
            active_endpoint_id: self.active_endpoint_id.clone(),
            histories: self.agent_history.clone(),
        });
        self.show_feedback("agent history saved", std::time::Instant::now());
        outcome.repaint = true;
    }

    /// Starts every machine's history over from its focused agent. The saved
    /// history stays.
    pub(super) fn reset_agent_history(&mut self, outcome: &mut ClientShellInput) {
        for history in self.agent_history.values_mut() {
            history.reset();
        }
        self.show_feedback("agent history reset", std::time::Instant::now());
        outcome.repaint = true;
    }

    /// Restores the saved history of every machine, then focuses the saved agent
    /// of the machine that was active, switching to that machine if needed.
    pub(super) fn restore_agent_history(&mut self, outcome: &mut ClientShellInput) {
        outcome.repaint = true;
        let Some(saved) = self.saved_agent_history.as_ref() else {
            self.show_feedback("no saved agent history", std::time::Instant::now());
            return;
        };
        for (endpoint_id, history) in &mut self.agent_history {
            history.restore(saved.histories.get(endpoint_id));
        }
        let endpoint_id = saved.active_endpoint_id.clone();
        let snapshot = if endpoint_id == self.active_endpoint_id {
            self.snapshot.as_deref()
        } else {
            self.endpoints
                .iter()
                .find(|endpoint| endpoint.endpoint_id == endpoint_id)
                .and_then(|endpoint| endpoint.snapshot.as_deref())
        };
        let pane_id = snapshot
            .zip(self.agent_history.get_mut(&endpoint_id))
            .and_then(|(snapshot, history)| history.step(snapshot, 0));
        // lessismore: switching machines resets the projection, which hides this toast.
        // Revisit when restore across machines needs visible confirmation.
        self.show_feedback("agent history restored", std::time::Instant::now());
        if let Some(pane_id) = pane_id {
            self.focus_or_activate(
                endpoint_id,
                ClientEndpointFocusTarget::Pane(pane_id),
                outcome,
            );
        }
    }
}

/// Browser-style history of the agent panes one client window focused on one
/// machine, oldest first. Back and forward move the cursor without adding entries;
/// a new selection drops the entries after the cursor.
#[derive(Debug, Default, Clone)]
pub(super) struct AgentHistory {
    /// Pane ids are only meaningful within one server boot.
    boot_id: String,
    entries: Vec<String>,
    cursor: usize,
    /// The focused agent pane in the last observed snapshot.
    focused_agent: Option<String>,
    /// Panes that back or forward asked to focus. Their focus can arrive after
    /// further key presses moved the cursor on, and must not count as a selection.
    pending: Vec<String>,
}

impl AgentHistory {
    /// Records the focused pane as a selection when focus newly lands on an agent.
    pub(super) fn observe(&mut self, snapshot: &ClientShellSnapshot) {
        if self.boot_id != snapshot.boot_id {
            *self = Self {
                boot_id: snapshot.boot_id.clone(),
                ..Self::default()
            };
        }
        let focused_agent = snapshot
            .focused_pane_id
            .clone()
            .filter(|pane_id| is_agent(snapshot, pane_id));
        if focused_agent == self.focused_agent {
            return;
        }
        self.focused_agent = focused_agent.clone();
        let Some(pane_id) = focused_agent else {
            return;
        };
        if self.entries.get(self.cursor) == Some(&pane_id) {
            self.pending.clear();
            return;
        }
        if self.pending.contains(&pane_id) {
            return;
        }
        self.pending.clear();
        self.entries.truncate(self.cursor + 1);
        self.entries.push(pane_id);
        if self.entries.len() > AGENT_HISTORY_LIMIT {
            self.entries.remove(0);
        }
        self.cursor = self.entries.len() - 1;
    }

    /// Forgets every selection but the focused agent, as if the window just opened.
    fn reset(&mut self) {
        self.entries = self.focused_agent.iter().cloned().collect();
        self.cursor = 0;
        self.pending.clear();
    }

    /// Takes the entries and position of `saved` but keeps the focus this history
    /// last observed, so the next snapshot is not mistaken for a selection. Saved
    /// entries from an earlier server boot name panes that no longer exist.
    fn restore(&mut self, saved: Option<&AgentHistory>) {
        let saved = saved.filter(|saved| saved.boot_id == self.boot_id);
        self.entries = saved.map(|saved| saved.entries.clone()).unwrap_or_default();
        self.cursor = saved.map_or(0, |saved| saved.cursor);
        self.pending.clear();
    }

    /// Moves the cursor back (`delta` -1) or forward (1) and returns the pane to
    /// focus; 0 returns the cursor entry. When focus is away from the cursor entry,
    /// for example on a shell, it returns the cursor entry instead of moving.
    pub(super) fn step(&mut self, snapshot: &ClientShellSnapshot, delta: isize) -> Option<String> {
        self.forget_gone_agents(snapshot);
        let current = self.entries.get(self.cursor)?;
        let away = self.pending.is_empty() && snapshot.focused_pane_id.as_ref() != Some(current);
        let cursor = if away {
            self.cursor
        } else {
            self.cursor.checked_add_signed(delta)?
        };
        let pane_id = self.entries.get(cursor)?.clone();
        self.cursor = cursor;
        self.pending.push(pane_id.clone());
        Some(pane_id)
    }

    /// Drops entries whose pane closed or no longer runs an agent, and repeats the
    /// removal leaves next to each other, so every step focuses a different agent.
    fn forget_gone_agents(&mut self, snapshot: &ClientShellSnapshot) {
        let cursor = self.cursor;
        let mut index = 0;
        let mut kept_up_to_cursor = 0usize;
        let mut previous: Option<String> = None;
        self.entries.retain(|pane_id| {
            let keep = is_agent(snapshot, pane_id) && previous.as_ref() != Some(pane_id);
            if keep {
                previous = Some(pane_id.clone());
                if index <= cursor {
                    kept_up_to_cursor += 1;
                }
            }
            index += 1;
            keep
        });
        self.cursor = kept_up_to_cursor.saturating_sub(1);
    }
}

fn is_agent(snapshot: &ClientShellSnapshot, pane_id: &str) -> bool {
    snapshot.agents.iter().any(|agent| agent.pane_id == pane_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::AgentStatus;
    use crate::protocol::ClientShellAgent;

    const AGENTS: &[&str] = &["a1", "a2", "a3", "a4"];

    fn snapshot(agents: &[&str], focused: &str) -> ClientShellSnapshot {
        let mut snapshot = super::super::tests::snapshot();
        snapshot.focused_pane_id = Some(focused.into());
        snapshot.agents = agents
            .iter()
            .map(|pane_id| ClientShellAgent {
                pane_id: (*pane_id).into(),
                workspace_id: "ws_1".into(),
                tab_id: "tab_1".into(),
                name: None,
                display_agent: None,
                agent: Some("claude".into()),
                title: None,
                terminal_title: None,
                terminal_title_stripped: None,
                agent_status: AgentStatus::Idle,
                state_change_seq: 1,
                state_labels: Vec::new(),
                tokens: Vec::new(),
                focused: *pane_id == focused,
            })
            .collect();
        snapshot
    }

    /// Focuses `pane_id` as a manual selection would.
    fn select(history: &mut AgentHistory, pane_id: &str) {
        history.observe(&snapshot(AGENTS, pane_id));
    }

    /// Presses back (-1) or forward (1) while `focused` has focus, then lets the
    /// focus request land.
    fn step(history: &mut AgentHistory, focused: &str, delta: isize) -> Option<String> {
        let target = history.step(&snapshot(AGENTS, focused), delta)?;
        history.observe(&snapshot(AGENTS, &target));
        Some(target)
    }

    fn entries(history: &AgentHistory) -> (Vec<&str>, usize) {
        (
            history.entries.iter().map(String::as_str).collect(),
            history.cursor,
        )
    }

    #[test]
    fn back_forward_and_a_new_selection_drop_the_entries_after_the_cursor() {
        let mut history = AgentHistory::default();
        for pane_id in ["a1", "a3", "a2", "a4"] {
            select(&mut history, pane_id);
        }
        assert_eq!(entries(&history), (vec!["a1", "a3", "a2", "a4"], 3));

        assert_eq!(step(&mut history, "a4", -1).as_deref(), Some("a2"));
        assert_eq!(step(&mut history, "a2", -1).as_deref(), Some("a3"));
        assert_eq!(step(&mut history, "a3", 1).as_deref(), Some("a2"));
        assert_eq!(entries(&history), (vec!["a1", "a3", "a2", "a4"], 2));

        select(&mut history, "a1");
        assert_eq!(entries(&history), (vec!["a1", "a3", "a2", "a1"], 3));
        assert_eq!(step(&mut history, "a1", 1), None);
    }

    #[test]
    fn keeps_the_last_fifty_selections() {
        let mut history = AgentHistory::default();
        for _ in 0..30 {
            select(&mut history, "a1");
            select(&mut history, "a2");
        }
        assert_eq!(history.entries.len(), AGENT_HISTORY_LIMIT);
        assert_eq!(history.cursor, AGENT_HISTORY_LIMIT - 1);
    }

    #[test]
    fn shells_are_not_selections_and_back_first_returns_to_the_cursor_agent() {
        let mut history = AgentHistory::default();
        select(&mut history, "a1");
        select(&mut history, "a2");
        // A shell pane is not an agent, so focusing it records nothing.
        history.observe(&snapshot(AGENTS, "shell"));
        assert_eq!(entries(&history), (vec!["a1", "a2"], 1));

        assert_eq!(step(&mut history, "shell", -1).as_deref(), Some("a2"));
        assert_eq!(step(&mut history, "a2", -1).as_deref(), Some("a1"));
    }

    #[test]
    fn an_agent_starting_in_the_focused_shell_is_a_selection() {
        let mut history = AgentHistory::default();
        select(&mut history, "a1");
        history.observe(&snapshot(AGENTS, "p5"));
        history.observe(&snapshot(&["a1", "a2", "a3", "a4", "p5"], "p5"));
        assert_eq!(entries(&history), (vec!["a1", "p5"], 1));
    }

    #[test]
    fn steps_skip_and_forget_agents_that_are_gone() {
        let mut history = AgentHistory::default();
        for pane_id in ["a1", "a2", "a3", "a1", "a4"] {
            select(&mut history, pane_id);
        }
        // a3 exited and a2's pane closed; the two a1 entries become neighbors.
        let remaining = ["a1", "a4"];
        let target = history.step(&snapshot(&remaining, "a4"), -1);
        assert_eq!(target.as_deref(), Some("a1"));
        assert_eq!(entries(&history), (vec!["a1", "a4"], 0));
    }

    #[test]
    fn quick_presses_do_not_record_the_panes_they_pass() {
        let mut history = AgentHistory::default();
        for pane_id in ["a1", "a3", "a2", "a4"] {
            select(&mut history, pane_id);
        }
        // Two backs before the first focus request lands.
        assert_eq!(
            history.step(&snapshot(AGENTS, "a4"), -1).as_deref(),
            Some("a2")
        );
        assert_eq!(
            history.step(&snapshot(AGENTS, "a4"), -1).as_deref(),
            Some("a3")
        );
        history.observe(&snapshot(AGENTS, "a2"));
        history.observe(&snapshot(AGENTS, "a3"));
        assert_eq!(entries(&history), (vec!["a1", "a3", "a2", "a4"], 1));
    }

    #[test]
    fn a_server_restart_clears_the_history() {
        let mut history = AgentHistory::default();
        select(&mut history, "a1");
        select(&mut history, "a2");
        let mut restarted = snapshot(AGENTS, "a3");
        restarted.boot_id = "boot-2".into();
        history.observe(&restarted);
        assert_eq!(entries(&history), (vec!["a3"], 0));
    }

    #[test]
    fn reset_keeps_only_the_focused_agent() {
        let mut history = AgentHistory::default();
        for pane_id in ["a1", "a2", "a3"] {
            select(&mut history, pane_id);
        }
        step(&mut history, "a3", -1);
        history.reset();
        assert_eq!(entries(&history), (vec!["a2"], 0));
        select(&mut history, "a4");
        assert_eq!(step(&mut history, "a4", -1).as_deref(), Some("a2"));
    }

    #[test]
    fn restore_returns_to_the_saved_entries_and_position_of_the_same_boot() {
        let mut history = AgentHistory::default();
        for pane_id in ["a1", "a2", "a3"] {
            select(&mut history, pane_id);
        }
        step(&mut history, "a3", -1);
        let saved = history.clone();
        select(&mut history, "a4");
        assert_eq!(entries(&history), (vec!["a1", "a2", "a4"], 2));

        history.restore(Some(&saved));
        assert_eq!(entries(&history), (vec!["a1", "a2", "a3"], 1));
        let target = history.step(&snapshot(AGENTS, "a4"), 0);
        assert_eq!(target.as_deref(), Some("a2"));
        // Neither the focus that a4 still has nor the restored focus is a selection.
        history.observe(&snapshot(AGENTS, "a4"));
        history.observe(&snapshot(AGENTS, "a2"));
        assert_eq!(entries(&history), (vec!["a1", "a2", "a3"], 1));

        let mut restarted = snapshot(AGENTS, "a1");
        restarted.boot_id = "boot-2".into();
        history.observe(&restarted);
        history.restore(Some(&saved));
        assert_eq!(entries(&history), (vec![], 0));
    }
}
