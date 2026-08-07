# Documentation style

Write public technical documentation in clear English.
Keep project records in their existing folders.

## File layout

| Folder | Purpose |
| --- | --- |
| `10-overview` | Stable concepts and architecture |
| `20-guides` | Installation and operation guides |
| `30-tasks` | Implementation history |
| `40-tests` | Detailed verification records |
| `50-bugs` | Bug investigations |
| `90-appendix` | Reference material |

Use English file names.
Use lower-case kebab-case for new files.

## Writing

Put one sentence on each source line.
Keep each paragraph between one and three sentences.
Use short words when they are accurate.
Explain a new term when it first appears.
Move commands and data into fenced code blocks.

Do not use YAML frontmatter in public documents.
Do not use Obsidian callouts, wiki links, or Dataview blocks.

## Links

Use relative Markdown links.
Relative links work on GitHub and in local editors.

```markdown
[MicroNetwork](../20-guides/explicit-micro-network.md)
```

Run the link checker after moving or editing documents.

```sh
python3 scripts/check-doc-links.py
```

## Recommended sections

A guide should use these sections when they are useful:

1. Purpose
2. Requirements
3. Steps
4. How it works
5. Checks
6. Troubleshooting

A reference should lead with the most common fields or commands.
Keep background information near the end.
