struct OpenVikingPrefetch {
    st: VikingState,
    q: String,
    session_id: String,
    recall: OpenVikingRecallConfig,
}

#[derive(Debug, Clone)]
struct OpenVikingRecallItem {
    uri: String,
    abstract_text: String,
    score: Option<f64>,
    level: Option<u64>,
}

impl OpenVikingPrefetch {
    fn run(self) -> Result<String, ()> {
        let h = viking_headers(&self.st);
        let deadline = Instant::now() + self.recall.timeout;
        let search = self
            .post_search(&h, deadline)
            .or_else(|_| self.post_find(&h, deadline))
            .map_err(|_| ())?;
        let mut items = extract_recall_items(&search, self.recall.include_resources);
        items.retain(|item| {
            item.score
                .map(|score| score >= self.recall.score_threshold)
                .unwrap_or(true)
        });
        rank_recall_items(&mut items, &self.q);
        dedup_recall_items(&mut items);
        let mut parts = Vec::new();
        let mut used_chars = 0usize;
        let mut full_reads = 0usize;
        for item in items.into_iter().take(self.recall.limit) {
            let mut text = item.abstract_text.trim().to_string();
            let needs_full_read = !self.recall.prefer_abstract
                && full_reads < self.recall.full_read_limit
                && (item.level.is_some_and(|level| level >= 2) || text.is_empty());
            if needs_full_read {
                if let Ok(value) = read_openviking_uri(
                    &self.st,
                    &h,
                    &item.uri,
                    "full",
                    Some(remaining_timeout(deadline, self.recall.request_timeout)?),
                ) {
                    if let Some(content) = recall_content_text(&value) {
                        text = truncate_chars(&content, READ_BATCH_FULL_LIMIT);
                        full_reads += 1;
                    }
                }
            }
            if text.is_empty() {
                continue;
            }
            let score = item
                .score
                .map(|score| format!("{score:.2}"))
                .unwrap_or_else(|| "n/a".to_string());
            let line = format!("- [{score}] {} ({})", text, item.uri);
            let next_len = line.chars().count() + 1;
            if used_chars.saturating_add(next_len) > self.recall.max_injected_chars {
                break;
            }
            used_chars += next_len;
            parts.push(line);
        }
        if parts.is_empty() {
            Err(())
        } else {
            Ok(parts.join("\n"))
        }
    }

    fn post_search(
        &self,
        headers: &reqwest::header::HeaderMap,
        deadline: Instant,
    ) -> Result<Value, ()> {
        let url = format!("{}/api/v1/search/search", self.st.endpoint);
        let context_type = if self.recall.include_resources {
            json!(["memory", "resource"])
        } else {
            json!("memory")
        };
        let mut body = json!({
            "query": self.q,
            "limit": self.recall.limit,
            "score_threshold": 0,
            "context_type": context_type,
        });
        if !self.session_id.trim().is_empty() {
            body["session_id"] = json!(self.session_id.trim());
        }
        let resp = self
            .st
            .client
            .post(&url)
            .headers(headers.clone())
            .timeout(remaining_timeout(deadline, self.recall.request_timeout)?)
            .json(&body)
            .send()
            .map_err(|_| ())?;
        if !resp.status().is_success() {
            return Err(());
        }
        resp.json::<Value>().map_err(|_| ())
    }

    fn post_find(
        &self,
        headers: &reqwest::header::HeaderMap,
        deadline: Instant,
    ) -> Result<Value, ()> {
        let url = format!("{}/api/v1/search/find", self.st.endpoint);
        let body = json!({"query": self.q, "top_k": self.recall.limit});
        let resp = self
            .st
            .client
            .post(&url)
            .headers(headers.clone())
            .timeout(remaining_timeout(deadline, self.recall.request_timeout)?)
            .json(&body)
            .send()
            .map_err(|_| ())?;
        if !resp.status().is_success() {
            return Err(());
        }
        resp.json::<Value>().map_err(|_| ())
    }
}

fn derive_openviking_user_text(query: &str) -> String {
    query
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn remaining_timeout(deadline: Instant, request_timeout: Duration) -> Result<Duration, ()> {
    let remaining = deadline.checked_duration_since(Instant::now()).ok_or(())?;
    if remaining <= RECALL_MIN_TIMEOUT {
        Err(())
    } else {
        Ok(remaining.min(request_timeout))
    }
}

fn extract_recall_items(value: &Value, include_resources: bool) -> Vec<OpenVikingRecallItem> {
    let mut out = Vec::new();
    for key in ["results", "memories"] {
        collect_recall_items_from_array(value, key, &mut out);
        if let Some(result) = value.get("result") {
            collect_recall_items_from_array(result, key, &mut out);
        }
    }
    if include_resources {
        collect_recall_items_from_array(value, "resources", &mut out);
        if let Some(result) = value.get("result") {
            collect_recall_items_from_array(result, "resources", &mut out);
        }
    }
    out
}

fn collect_recall_items_from_array(value: &Value, key: &str, out: &mut Vec<OpenVikingRecallItem>) {
    let Some(items) = value.get(key).and_then(Value::as_array) else {
        return;
    };
    for item in items {
        let Some(uri) = item
            .get("uri")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|uri| !uri.is_empty())
        else {
            continue;
        };
        let abstract_text = ["abstract", "summary", "text", "content"]
            .iter()
            .find_map(|key| item.get(*key).and_then(Value::as_str))
            .unwrap_or_default()
            .trim()
            .to_string();
        out.push(OpenVikingRecallItem {
            uri: uri.to_string(),
            abstract_text,
            score: item.get("score").and_then(Value::as_f64),
            level: item
                .get("level")
                .or_else(|| item.get("content_level"))
                .and_then(Value::as_u64),
        });
    }
}

fn rank_recall_items(items: &mut [OpenVikingRecallItem], query: &str) {
    let query_tokens = query_tokens(query);
    items.sort_by(|a, b| {
        let b_score = recall_rank_score(b, &query_tokens);
        let a_score = recall_rank_score(a, &query_tokens);
        b_score
            .partial_cmp(&a_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

fn query_tokens(query: &str) -> HashSet<String> {
    query
        .split(|ch: char| !ch.is_alphanumeric())
        .map(str::trim)
        .filter(|token| token.len() >= 3)
        .map(str::to_ascii_lowercase)
        .collect()
}

fn recall_rank_score(item: &OpenVikingRecallItem, query_tokens: &HashSet<String>) -> f64 {
    let mut score = item.score.unwrap_or(0.0);
    if !query_tokens.is_empty() {
        let haystack = format!("{} {}", item.uri, item.abstract_text).to_ascii_lowercase();
        let overlap = query_tokens
            .iter()
            .filter(|token| haystack.contains(token.as_str()))
            .count() as f64;
        score += overlap * 0.05;
    }
    if item
        .uri
        .rsplit('/')
        .next()
        .is_some_and(|leaf| !leaf.is_empty())
    {
        score += 0.01;
    }
    score
}

fn dedup_recall_items(items: &mut Vec<OpenVikingRecallItem>) {
    let mut seen = HashSet::new();
    items.retain(|item| seen.insert(item.uri.clone()));
}

fn read_openviking_uri(
    st: &VikingState,
    headers: &reqwest::header::HeaderMap,
    uri: &str,
    level: &str,
    timeout: Option<Duration>,
) -> Result<Value, String> {
    let path = match level {
        "abstract" => "/api/v1/content/abstract",
        "full" => "/api/v1/content/read",
        _ => "/api/v1/content/overview",
    };
    let url = format!("{}{}", st.endpoint, path);
    let mut request = st
        .client
        .get(&url)
        .headers(headers.clone())
        .query(&[("uri", uri)]);
    if let Some(timeout) = timeout {
        request = request.timeout(timeout);
    }
    match request.send() {
        Ok(resp) if resp.status().is_success() => resp
            .json::<Value>()
            .map_err(|e| format!("OpenViking read JSON: {e}")),
        Ok(resp) => Err(format!("HTTP {}", resp.status())),
        Err(e) => Err(e.to_string()),
    }
}

fn recall_content_text(value: &Value) -> Option<String> {
    value
        .get("content")
        .or_else(|| value.get("text"))
        .or_else(|| value.get("body"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| {
            value
                .get("result")
                .and_then(|result| {
                    result
                        .get("content")
                        .or_else(|| result.get("text"))
                        .or_else(|| result.get("body"))
                })
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
}

fn truncate_chars(value: &str, limit: usize) -> String {
    let mut out = value.chars().take(limit).collect::<String>();
    if value.chars().count() > limit {
        out.push_str("...");
    }
    out
}
