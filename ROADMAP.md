# Roadmap

Each milestone is a version of ShellCraft that can be installed and used. The milestones come in
this order, and their scope can change as the work goes on.

| Milestone | What it adds |
|-----------|--------------|
| M0 | The workspace, the checks and CI, the first shared crates, the command registry with `shellcraft-cli` and its MCP server, and a window with one terminal running the login shell |
| M1 | The terminal core: a separate process for each session, the grid and its updates, a GPU renderer and keyboard input |
| M2 | A companion window that runs beside another terminal: the user's model provider accounts and how much of each limit is left, a monitor for remote build hosts, and read-only panels for a coordination connector |
| M3 | Tabs, splits, profiles, themes, find and a command palette |
| M4 | Sessions that survive a crash, an update or a quit: a session journal, snapshots and restore |
| M5 | Text quality and input: fonts, emoji, grapheme clusters, input methods, the kitty keyboard protocol, mouse, copy mode and accessibility |
| M6 | Shell integration, command history and notifications |
| M7 | Projects: a project identity that holds across folders and machines, the rule files that apply in each folder, profiles and guards per project, and the user's global ignore files |
| M8 | A built-in coding agent that works on several subscriptions, with conversation history and project memory |
| M9 | Policy and automation: caller classes, approvals, grants, triggers and schedules |
| M10 | Remote hosts and remote builds in tabs |
| M11 | Coordination work in tabs |
| M12 | A local encrypted vault and the use of its secrets in sessions |
| M13 | End-to-end encrypted sync of settings, secrets, sign-ins and project memory between the user's computers |
| M14 | Remote sessions that survive sleep and network changes, and handoff between computers |
| M15 | Connections: tmux control mode, SFTP, port forwarding, serial ports and Telnet |
| M16 | Later work: inline images, a read-only board, and a portable mode |

Before the first alpha release, ShellCraft also gets an updater that installs signed releases from
this repository's GitHub Releases.
