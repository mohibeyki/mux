/// Parallel command expansion.
///
/// Syntax:
///   [name=range] ...more prefixes... command with {name} placeholders
///
/// Range types:
///   [shard=1-64]         → numeric: "1", "2", ..., "64"
///   [shard=01-64]        → zero-padded: "01", "02", ..., "64"
///   [region=east,west]   → list: "east", "west"
///
/// Combination modes:
///   Separate [...] blocks → cross product
///     [shard=1-3] [region=a,b] cmd     → 6 commands (1,a), (1,b), (2,a), ...
///
///   Space-separated names in one [...] → zip (must be same length)
///     [shard=1-3 region=a,b,c] cmd     → 3 commands (1,a), (2,b), (3,c)

/// A single named parameter with its expanded values
#[derive(Debug, Clone)]
pub struct ParamDef {
    pub name: String,
    pub values: Vec<String>,
}

/// A group of parameters. Params within the same group are zipped.
/// Separate groups are cross-producted.
#[derive(Debug, Clone)]
pub struct ParamGroup {
    pub params: Vec<ParamDef>,
}

/// Result of parsing a command for parallel expansion
#[derive(Debug)]
pub struct ParsedParallel {
    /// Parameter groups (cross-producted between groups, zipped within)
    pub groups: Vec<ParamGroup>,
    /// The command template with {name} placeholders
    pub template: String,
}

/// A single expanded command with its parameter assignments
#[derive(Debug)]
pub struct ExpandedCommand {
    /// The fully substituted command string
    pub command: String,
    /// Display label: e.g., "[n=14][region=pnb]"
    pub label: String,
}

/// Parse a range string into a list of values.
/// "1-64" → ["1", "2", ..., "64"]
/// "01-64" → ["01", "02", ..., "64"] (zero-padded)
/// "east,west" → ["east", "west"]
fn parse_range(range: &str) -> Option<Vec<String>> {
    // Check for comma-separated list first
    if range.contains(',') {
        return Some(range.split(',').map(|s| s.trim().to_string()).collect());
    }

    // Check for numeric range: n-m
    if let Some((start_str, end_str)) = range.split_once('-') {
        let start: i64 = start_str.parse().ok()?;
        let end: i64 = end_str.parse().ok()?;

        if start > end {
            return None;
        }

        // Detect zero-padding: if the start string has leading zeros
        let pad_width = if start_str.len() > 1 && start_str.starts_with('0') {
            // Pad to the width of the longer of start/end
            start_str.len().max(end_str.len())
        } else {
            0
        };

        let values: Vec<String> = (start..=end)
            .map(|n| {
                if pad_width > 0 {
                    format!("{:0>width$}", n, width = pad_width)
                } else {
                    n.to_string()
                }
            })
            .collect();

        return Some(values);
    }

    // Single value
    Some(vec![range.to_string()])
}

/// Parse a single [...] block into a ParamGroup.
/// "[shard=1-3]" → ParamGroup with one param
/// "[shard=1-3 region=a,b,c]" → ParamGroup with two zipped params
fn parse_bracket_block(block: &str) -> Option<ParamGroup> {
    let inner = block.strip_prefix('[')?.strip_suffix(']')?;
    let mut params = Vec::new();

    // Split on whitespace for multiple params (zip mode)
    for part in inner.split_whitespace() {
        let (name, range) = part.split_once('=')?;
        let values = parse_range(range)?;
        params.push(ParamDef {
            name: name.to_string(),
            values,
        });
    }

    // Validate zip: all params in the same group must have the same length
    if params.len() > 1 {
        let len = params[0].values.len();
        if params.iter().any(|p| p.values.len() != len) {
            return None; // mismatched lengths
        }
    }

    if params.is_empty() {
        return None;
    }

    Some(ParamGroup { params })
}

/// Parse a full input string for parallel expansion.
/// Returns None if the input has no [...] prefixes (normal command).
pub fn parse_parallel(input: &str) -> Option<ParsedParallel> {
    let trimmed = input.trim();

    // Quick check: must start with '['
    if !trimmed.starts_with('[') {
        return None;
    }

    let mut remaining = trimmed;
    let mut groups = Vec::new();

    // Parse consecutive [...] blocks from the start
    while remaining.starts_with('[') {
        // Find the matching ']'
        let close = remaining.find(']')?;
        let block = &remaining[..=close];

        let group = parse_bracket_block(block)?;
        groups.push(group);

        remaining = remaining[close + 1..].trim_start();
    }

    if groups.is_empty() || remaining.is_empty() {
        return None;
    }

    Some(ParsedParallel {
        groups,
        template: remaining.to_string(),
    })
}

/// Expand a ParsedParallel into a list of concrete commands.
/// Groups are cross-producted; params within a group are zipped.
pub fn expand(parsed: &ParsedParallel) -> Vec<ExpandedCommand> {
    // Each group produces a list of "rows" (one row per zip iteration).
    // A row is a Vec<(name, value)>.
    let group_rows: Vec<Vec<Vec<(String, String)>>> = parsed
        .groups
        .iter()
        .map(|group| {
            let len = group.params[0].values.len();
            (0..len)
                .map(|i| {
                    group
                        .params
                        .iter()
                        .map(|p| (p.name.clone(), p.values[i].clone()))
                        .collect()
                })
                .collect()
        })
        .collect();

    // Cross-product all groups
    let mut combinations: Vec<Vec<(String, String)>> = vec![vec![]];
    for group in &group_rows {
        let mut new_combos = Vec::new();
        for existing in &combinations {
            for row in group {
                let mut combined = existing.clone();
                combined.extend(row.iter().cloned());
                new_combos.push(combined);
            }
        }
        combinations = new_combos;
    }

    // Substitute into template
    combinations
        .into_iter()
        .map(|assignments| {
            let mut command = parsed.template.clone();
            let mut label = String::new();

            for (name, value) in &assignments {
                command = command.replace(&format!("{{{}}}", name), value);
                if parsed.groups.len() == 1 && parsed.groups[0].params.len() == 1 {
                    command = command.replace("{}", value);
                }
                label.push_str(&format!("[{}={}]", name, value));
            }

            ExpandedCommand {
                command,
                label,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_range_numeric() {
        let vals = parse_range("1-5").unwrap();
        assert_eq!(vals, vec!["1", "2", "3", "4", "5"]);
    }

    #[test]
    fn test_parse_range_zero_padded() {
        let vals = parse_range("01-05").unwrap();
        assert_eq!(vals, vec!["01", "02", "03", "04", "05"]);
    }

    #[test]
    fn test_parse_range_list() {
        let vals = parse_range("east,west,staging").unwrap();
        assert_eq!(vals, vec!["east", "west", "staging"]);
    }

    #[test]
    fn test_parse_parallel_single_param() {
        let parsed = parse_parallel("[shard=1-3] mysql -h shard-{shard}").unwrap();
        assert_eq!(parsed.groups.len(), 1);
        assert_eq!(parsed.groups[0].params[0].name, "shard");
        assert_eq!(parsed.groups[0].params[0].values, vec!["1", "2", "3"]);
        assert_eq!(parsed.template, "mysql -h shard-{shard}");
    }

    #[test]
    fn test_parse_parallel_cross_product() {
        let parsed =
            parse_parallel("[shard=1-2] [region=east,west] cmd -s {shard} -r {region}").unwrap();
        assert_eq!(parsed.groups.len(), 2);
        assert_eq!(parsed.groups[0].params[0].values, vec!["1", "2"]);
        assert_eq!(parsed.groups[1].params[0].values, vec!["east", "west"]);
    }

    #[test]
    fn test_parse_parallel_zip() {
        let parsed =
            parse_parallel("[shard=1-3 region=a,b,c] cmd {shard} {region}").unwrap();
        assert_eq!(parsed.groups.len(), 1);
        assert_eq!(parsed.groups[0].params.len(), 2);
        assert_eq!(parsed.groups[0].params[0].name, "shard");
        assert_eq!(parsed.groups[0].params[1].name, "region");
    }

    #[test]
    fn test_parse_parallel_zip_mismatched_length() {
        // Zip with different lengths should fail
        let result = parse_parallel("[shard=1-3 region=a,b] cmd {shard} {region}");
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_parallel_not_parallel() {
        assert!(parse_parallel("echo hello").is_none());
        assert!(parse_parallel("ls -la").is_none());
    }

    #[test]
    fn test_expand_single_param() {
        let parsed = parse_parallel("[n=1-3] echo {n}").unwrap();
        let expanded = expand(&parsed);
        assert_eq!(expanded.len(), 3);
        assert_eq!(expanded[0].command, "echo 1");
        assert_eq!(expanded[0].label, "[n=1]");
        assert_eq!(expanded[1].command, "echo 2");
        assert_eq!(expanded[2].command, "echo 3");
    }

    #[test]
    fn test_expand_cross_product() {
        let parsed = parse_parallel("[a=1-2] [b=x,y] cmd {a} {b}").unwrap();
        let expanded = expand(&parsed);
        assert_eq!(expanded.len(), 4); // 2 x 2
        assert_eq!(expanded[0].command, "cmd 1 x");
        assert_eq!(expanded[1].command, "cmd 1 y");
        assert_eq!(expanded[2].command, "cmd 2 x");
        assert_eq!(expanded[3].command, "cmd 2 y");
    }

    #[test]
    fn test_expand_zip() {
        let parsed = parse_parallel("[a=1-3 b=x,y,z] cmd {a} {b}").unwrap();
        let expanded = expand(&parsed);
        assert_eq!(expanded.len(), 3); // zipped, not cross product
        assert_eq!(expanded[0].command, "cmd 1 x");
        assert_eq!(expanded[1].command, "cmd 2 y");
        assert_eq!(expanded[2].command, "cmd 3 z");
    }

    #[test]
    fn test_expand_zero_padded() {
        let parsed = parse_parallel("[n=01-03] echo {n}").unwrap();
        let expanded = expand(&parsed);
        assert_eq!(expanded[0].command, "echo 01");
        assert_eq!(expanded[1].command, "echo 02");
        assert_eq!(expanded[2].command, "echo 03");
    }

}
