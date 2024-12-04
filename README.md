# Timekeeper - A TUI for TimeFlip2

The Timekeeper allows to visualize and categorize the data logged by TimeFlip2[^1].
It allows to add notes to your tracked time and optionally book the time in JIRA.

# Configuration

We rely on timeflippers[^2] to communicate with the dice. Please take a look to
their README for instructions on how to get the TimeFlip2 working.
The `timeflip.toml` which is used for configuration of timeflippers is extended by
some configuration options to make it possible to specify the correct JIRA connection.
Use `jira_user`, `jira_token`, `jira_base_url` and `jira_board_id` for basic configuration.

`jira_jql_issue_filter` allows to specify a filter which items in the sprint are retrieved
  and used for the summary matching (see below).

`[[sides_jira_matching]]` allows to extend a timeflip-side with an regular expression
  `jira_summary_regex` to look for an issue in the sprint items with a specific summary.
  If the configured regular expression matches the summary of an item, this JIRA-ID is
  used for the booking suggestion.

`booker_issue_regex` allows to specify a regular expression which is applied to the notes
which have been added to an logged entry. If the notes contain such an expression, the
Timekeeper tries to retrieve information for this JIRA-ID and provide it in the booking
suggestion.
  ```
  jira_user = "your-user@domain.com"
  jira_token = "your-token"
  jira_base_url = "https://your-domain.atlassian.net"
  jira_board_id = "999"
  jira_jql_issue_filter = ""
  booker_issue_regex = 'PROJECT\-[0-9]+'

  [[sides_additional_info]]
  facet = 1
  jira_summary_regex = "Meetings"
  ```

---

[^1][https://timeflip.io/]

[^2][https://github.com/bzobl/timeflippers]
