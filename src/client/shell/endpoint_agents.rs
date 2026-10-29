use super::agent_sidebar::AgentPanelEntry;
use super::render::put_text;
use super::*;

pub(super) fn render_collapsed(
    buffer: &mut Buffer,
    area: Rect,
    endpoints: &[ClientShellEndpoint],
    active_endpoint_id: &ClientEndpointId,
    config: &ClientShellConfig,
    hits: &mut ShellHitMap,
) {
    let rows = agent_rows(endpoints, active_endpoint_id, config);
    for (index, row) in rows.into_iter().take(area.height as usize).enumerate() {
        let rect = Rect::new(area.x, area.y + index as u16, area.width, 1);
        if row.agent.focused {
            buffer.set_style(rect, Style::default().bg(config.palette.active_row_bg));
        }
        let initial = row.machine_label.chars().next().unwrap_or('?');
        put_text(
            buffer,
            rect.x,
            rect.y,
            rect.width,
            &format!(
                "{initial}{}",
                status_icon(row.agent.status, config.status_indicators)
            ),
            Style::default()
                .fg(if row.stale {
                    config.palette.overlay0
                } else {
                    status_color(row.agent.status, &config.palette)
                })
                .add_modifier(if row.stale {
                    Modifier::DIM
                } else {
                    Modifier::empty()
                }),
        );
        hits.endpoint_agents
            .push((rect, row.endpoint_id, row.agent.pane_id));
    }
}

pub(super) fn render_expanded(
    buffer: &mut Buffer,
    area: Rect,
    agent_view_label: Option<&str>,
    endpoints: &[ClientShellEndpoint],
    active_endpoint_id: &ClientEndpointId,
    config: &ClientShellConfig,
    folds: &super::agent_sidebar::AgentWorkspaceFolds,
    agent_scroll: &mut usize,
    hits: &mut ShellHitMap,
) {
    if !super::agent_sidebar::render_agent_panel_header(
        buffer,
        area,
        agent_view_label,
        config,
        hits,
    ) {
        return;
    }
    let entries = agent_entries(
        endpoints,
        active_endpoint_id,
        agent_view_label,
        config,
        folds,
    );
    super::agent_sidebar::render_agent_entries(
        buffer,
        area,
        &entries,
        agent_view_label.map(|_| " no matching agents"),
        config,
        agent_scroll,
        hits,
        |row| &row.agent,
        |buffer, rect, row, hits| {
            if row.stale {
                buffer.set_style(
                    rect,
                    Style::default()
                        .fg(config.palette.overlay0)
                        .add_modifier(Modifier::DIM),
                );
            }
            hits.endpoint_agents
                .push((rect, row.endpoint_id.clone(), row.agent.pane_id.clone()));
        },
    );
}

impl ClientShellState {
    pub(super) fn reveal_endpoint_agent(
        &mut self,
        endpoint_id: &ClientEndpointId,
        pane_id: &str,
        body_height: u16,
    ) {
        if body_height == 0 {
            return;
        }
        let Some(workspace_id) = self
            .endpoints
            .iter()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
            .and_then(|endpoint| endpoint.snapshot.as_deref())
            .and_then(|snapshot| {
                snapshot
                    .agents
                    .iter()
                    .find(|agent| agent.pane_id == pane_id)
            })
            .map(|agent| agent.workspace_id.as_str())
        else {
            return;
        };
        let entries = agent_entries(
            &self.endpoints,
            &self.active_endpoint_id,
            self.snapshot
                .as_deref()
                .and_then(|snapshot| snapshot.agent_view_label.as_deref()),
            &self.config,
            &self.collapsed_agent_workspaces,
        );
        // An agent in a folded workspace shows as its group header.
        let Some(target) = entries.iter().position(|entry| match entry {
            AgentPanelEntry::Agent(row) => {
                &row.endpoint_id == endpoint_id && row.agent.pane_id == pane_id
            }
            AgentPanelEntry::Group(header) => {
                header.collapsed
                    && &header.endpoint_id == endpoint_id
                    && header.workspace_id == workspace_id
            }
        }) else {
            return;
        };
        let heights = entries
            .iter()
            .map(|entry| {
                entry
                    .lines(self.config.agents.group_gap, |row| row.agent.rows.len())
                    .max(1)
                    .min(u16::MAX as usize) as u16
            })
            .collect::<Vec<_>>();
        let mut gaps = vec![self.config.agents.row_gap; entries.len()];
        if let Some(last) = gaps.last_mut() {
            *last = 0;
        }
        self.agent_scroll = super::scroll::list_scroll_start_to_reveal(
            &heights,
            &gaps,
            body_height,
            self.agent_scroll,
            target,
        );
    }
}

struct EndpointAgentRow<'a> {
    endpoint_id: ClientEndpointId,
    machine_label: String,
    snapshot: &'a ClientShellSnapshot,
    stale: bool,
    agent: super::agent_sidebar::AgentRow<'a>,
}

fn agent_entries<'a>(
    endpoints: &'a [ClientShellEndpoint],
    active_endpoint_id: &ClientEndpointId,
    agent_view_label: Option<&str>,
    config: &ClientShellConfig,
    folds: &super::agent_sidebar::AgentWorkspaceFolds,
) -> Vec<AgentPanelEntry<EndpointAgentRow<'a>>> {
    let rows = agent_rows(endpoints, active_endpoint_id, config);
    if !super::agent_sidebar::groups_agents(config.agent_panel_sort, agent_view_label) {
        return rows.into_iter().map(AgentPanelEntry::Agent).collect();
    }
    super::agent_sidebar::group_agent_rows(
        rows,
        folds,
        |row| (&row.endpoint_id, &row.agent),
        |row| {
            let workspace =
                super::agent_sidebar::workspace_label(row.snapshot, row.agent.workspace_id);
            format!("{} · {workspace}", row.machine_label)
        },
    )
}

fn agent_rows<'a>(
    endpoints: &'a [ClientShellEndpoint],
    active_endpoint_id: &ClientEndpointId,
    config: &ClientShellConfig,
) -> Vec<EndpointAgentRow<'a>> {
    let mut rendered_rows = endpoints
        .iter()
        .filter_map(|endpoint| {
            endpoint.snapshot.as_deref().map(|snapshot| {
                snapshot
                    .agents
                    .iter()
                    .filter_map(|agent| {
                        super::agent_sidebar::agent_row(
                            snapshot,
                            &agent.pane_id,
                            config,
                            Some(&endpoint.label),
                        )
                    })
                    .map(|agent| ((endpoint.endpoint_id.clone(), agent.pane_id.clone()), agent))
                    .collect::<Vec<_>>()
            })
        })
        .flatten()
        .collect::<HashMap<_, _>>();

    super::aggregate_navigation::aggregate_agent_rows(
        endpoints,
        active_endpoint_id,
        config.agent_panel_sort,
    )
    .into_iter()
    .filter_map(|row| {
        let key = (row.endpoint.endpoint_id.clone(), row.agent.pane_id.clone());
        let mut agent = rendered_rows.remove(&key)?;
        agent.focused &= row.endpoint.endpoint_id == active_endpoint_id;
        Some(EndpointAgentRow {
            endpoint_id: row.endpoint.endpoint_id.clone(),
            machine_label: row.endpoint.label.to_owned(),
            snapshot: row.endpoint.snapshot,
            stale: row.endpoint.stale(),
            agent,
        })
    })
    .collect()
}
