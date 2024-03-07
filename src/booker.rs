use anyhow::bail;
use chrono::{offset::TimeZone, DateTime, NaiveDate, NaiveTime, Utc};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JiraAnswer {
    start_at: usize,
    max_results: usize,
    total: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EntryQuery {
    #[serde(flatten)]
    jira: JiraAnswer,
    issues: Vec<Entry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub key: String,
    pub fields: Fields,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Fields {
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SprintQuery {
    #[serde(flatten)]
    jira: JiraAnswer,
    values: Vec<Sprint>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Sprint {
    start_date: DateTime<Utc>,
    end_date: DateTime<Utc>,
    goal: String,
    id: usize,
    name: String,
}

async fn post_request<T: Serialize + ?Sized>(
    url: &str,
    path: &str,
    user: &str,
    token: &str,
    json: &T,
) -> anyhow::Result<(u16, Value)> {
    let url = format!("{}/rest/api/latest{}", url, path);
    let client = Client::new();
    let res = client
        .post(url)
        .basic_auth(user, Some(token))
        .json(json)
        .send()
        .await?;
    let status = res.status().as_u16();
    let data = res.text().await?;
    if status >= 400 {
        bail!("API call failed with status {}: {}", status, data);
    }
    return Ok((status, serde_json::from_str(&data)?));
}

async fn get_request(
    url: &str,
    path: &str,
    user: &str,
    token: &str,
    query: &[(&str, &str)],
) -> anyhow::Result<(u16, Value)> {
    let url = format!("{}/rest/agile/1.0{}", url, path);
    let client = Client::new();
    let res = client
        .get(url)
        .basic_auth(user, Some(token))
        .query(query)
        .send()
        .await?;
    let status = res.status().as_u16();
    let data = res.text().await?;
    if status >= 400 {
        bail!("API call failed with status {}: {}", status, data);
    }
    return Ok((status, serde_json::from_str(&data)?));
}

fn check_sprint_active(
    day_to_check: &NaiveDate,
    sprint_start: DateTime<Utc>,
    sprint_end: DateTime<Utc>,
) -> bool {
    let start = NaiveTime::from_hms_milli_opt(00, 00, 00, 0000).unwrap();
    let end = NaiveTime::from_hms_milli_opt(23, 59, 59, 999).unwrap();

    let day_start_timestamp = Utc.from_utc_datetime(&day_to_check.and_time(start));
    let day_end_timestamp = Utc.from_utc_datetime(&day_to_check.and_time(end));

    (sprint_end > day_start_timestamp && sprint_end < day_end_timestamp)
        || (sprint_start > day_start_timestamp && sprint_start < day_end_timestamp)
        || (sprint_start < day_start_timestamp && sprint_end >= day_end_timestamp)
}

pub async fn get_sprint_issues_for_date(
    base_url: &str,
    board_id: &str,
    jira_user: &str,
    jira_token: &str,
    day_to_query: &NaiveDate,
    issue_jql_filter: &str,
) -> anyhow::Result<Vec<Entry>> {
    let mut queried_sprints = vec![];
    let mut start_with = 0;
    let mut need_more_data = true;
    while need_more_data {
        let (_status, data) = get_request(
            base_url,
            &format!("/board/{}/sprint", board_id),
            &jira_user,
            &jira_token,
            &[("startAt", &start_with.to_string())],
        )
        .await?;
        let res: SprintQuery = serde_json::from_value(data)?;
        need_more_data = (res.values.len() + queried_sprints.len()) < res.jira.total;
        queried_sprints.extend_from_slice(&res.values);
        start_with = queried_sprints.len()
    }

    let current_sprint: Vec<_> = queried_sprints
        .iter()
        .filter_map(|e| {
            if check_sprint_active(day_to_query, e.start_date, e.end_date) {
                Some(e.id)
            } else {
                None
            }
        })
        .collect();

    let mut queried_issues = vec![];
    for id in current_sprint {
        let mut start_with = 0;
        let mut need_more_data = true;
        while need_more_data {
            let (_status, data) = get_request(
                base_url,
                &format!("/sprint/{}/issue", id),
                &jira_user,
                &jira_token,
                &[
                    ("jql", issue_jql_filter),
                    ("fields", "summary"),
                    ("maxResults", "2000"),
                    ("startAt", &start_with.to_string()),
                ],
            )
            .await?;
            let res: EntryQuery = serde_json::from_value(data)?;
            need_more_data = (res.issues.len() + queried_issues.len()) < res.jira.total;
            queried_issues.extend_from_slice(&res.issues);
            start_with = queried_issues.len()
        }
    }
    Ok(queried_issues)
}

pub async fn get_issues(
    base_url: &str,
    jira_user: &str,
    jira_token: &str,
    issues: Vec<String>,
) -> Vec<Entry> {
    let mut queried_issues = vec![];
    for id in issues {
        if let Ok((_status, data)) = get_request(
            base_url,
            &format!("/issue/{}", id),
            &jira_user,
            &jira_token,
            &[("fields", "summary")],
        )
        .await
        {
            if let Ok(entry) = serde_json::from_value(data) {
                queried_issues.push(entry);
            }
        }
    }
    queried_issues
}

pub async fn book_time(
    base_url: &str,
    jira_user: &str,
    jira_token: &str,
    issue: String,
    date: NaiveDate,
    time_spent: Duration,
) -> anyhow::Result<()> {
    post_request(
            base_url,
            &format!("/issue/{}/worklog", issue),
            &jira_user,
            &jira_token,
            &json!({"started": format!("{}", date.and_hms_opt(9,0,0).expect("valid time").and_utc().format("%FT%T%.3f%z")),
                    "timeSpentSeconds": time_spent.as_secs()}),
        )
        .await?;
    Ok(())
}
