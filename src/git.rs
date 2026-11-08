//! Thin async wrappers around the `git` and `gh` command line tools.

use serde::Deserialize;
use std::path::{Path, PathBuf};
use tokio::process::Command;

/// Runs a command in `dir` and returns stdout, or a descriptive error containing stderr.
async fn run(dir: &Path, program: &str, args: &[&str]) -> Result<String, String> {
    let bytes = run_bytes(dir, program, args).await?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

async fn run_bytes(dir: &Path, program: &str, args: &[&str]) -> Result<Vec<u8>, String> {
    crate::debug::dlog!("[git] running `{program} {}` in {}", args.join(" "), dir.display());
    let output = Command::new(program)
        .args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GH_PROMPT_DISABLED", "1")
        .env("NO_COLOR", "1")
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .map_err(|e| format!("Failed to run `{program}`: {e}"))?;
    crate::debug::dlog!("[git] `{program} {}` finished: {}", args.join(" "), output.status);

    if output.status.success() {
        Ok(output.stdout)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let detail = if stderr.trim().is_empty() { stdout.trim() } else { stderr.trim() };
        Err(format!(
            "`{program} {}` failed ({}):\n{}",
            args.join(" "),
            output.status,
            detail
        ))
    }
}

async fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    run(dir, "git", args).await
}

/// Verifies that `path` is inside a git work tree and returns the work tree root.
pub async fn verify_repo(path: PathBuf) -> Result<PathBuf, String> {
    if !path.is_dir() {
        return Err(format!("{} is not a directory", path.display()));
    }
    let inside = git(&path, &["rev-parse", "--is-inside-work-tree"])
        .await
        .map_err(|e| format!("{} is not a Git repository.\n\n{e}", path.display()))?;
    if inside.trim() != "true" {
        return Err(format!("{} is not inside a Git work tree", path.display()));
    }
    let top = git(&path, &["rev-parse", "--show-toplevel"]).await?;
    Ok(PathBuf::from(top.trim()))
}

/// Returns the list of tracked files with uncommitted changes (empty when clean).
pub async fn uncommitted_changes(repo: PathBuf) -> Result<Vec<String>, String> {
    let out = git(&repo, &["status", "--porcelain", "--untracked-files=no"]).await?;
    Ok(out.lines().filter(|l| !l.trim().is_empty()).map(str::to_owned).collect())
}

pub async fn current_branch(repo: &Path) -> Option<String> {
    git(repo, &["rev-parse", "--abbrev-ref", "HEAD"]).await.ok().map(|s| s.trim().to_owned())
}

/// Picks the remote to use: `origin` if present, otherwise the first one.
async fn remote_name(repo: &Path) -> Result<String, String> {
    let remotes = git(repo, &["remote"]).await?;
    let remotes: Vec<&str> = remotes.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if remotes.contains(&"origin") {
        Ok("origin".into())
    } else {
        remotes
            .first()
            .map(|r| r.to_string())
            .ok_or_else(|| "Repository has no remotes".to_string())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Author {
    pub login: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Label {
    pub name: String,
    #[serde(default)]
    pub color: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub head_ref_name: String,
    pub base_ref_name: String,
    pub author: Author,
    #[serde(default)]
    pub is_draft: bool,
    pub created_at: String,
    #[serde(default)]
    pub additions: u64,
    #[serde(default)]
    pub deletions: u64,
    #[serde(default)]
    pub changed_files: u64,
    #[serde(default)]
    pub labels: Vec<Label>,
    #[serde(default)]
    pub is_cross_repository: bool,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub body: String,
    /// (position, size) within a stack of PRs, 1-based. Computed locally.
    #[serde(skip)]
    pub stack: Option<(usize, usize)>,
}

/// Lists open PRs for the repo via `gh`, newest first, with stack info computed.
pub async fn list_prs(repo: PathBuf) -> Result<Vec<PullRequest>, String> {
    let json = run(
        &repo,
        "gh",
        &[
            "pr",
            "list",
            "--state",
            "open",
            "--limit",
            "200",
            "--json",
            "number,title,headRefName,baseRefName,author,isDraft,createdAt,additions,deletions,changedFiles,labels,isCrossRepository,url,body",
        ],
    )
    .await
    .map_err(|e| format!("Could not fetch pull requests with `gh`.\n\n{e}"))?;
    let mut prs: Vec<PullRequest> =
        serde_json::from_str(&json).map_err(|e| format!("Unexpected `gh` output: {e}"))?;
    compute_stacks(&mut prs);
    Ok(prs)
}

/// A PR is part of a stack when its base branch is another open PR's head branch
/// (or vice versa). Assigns 1-based positions counted from the bottom of the stack.
pub fn compute_stacks(prs: &mut [PullRequest]) {
    use std::collections::HashMap;
    let by_head: HashMap<String, usize> = prs
        .iter()
        .enumerate()
        .filter(|(_, p)| !p.is_cross_repository)
        .map(|(i, p)| (p.head_ref_name.clone(), i))
        .collect();
    let mut children: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut parent: Vec<Option<usize>> = vec![None; prs.len()];
    for (i, p) in prs.iter().enumerate() {
        if let Some(&j) = by_head.get(&p.base_ref_name)
            && j != i {
                parent[i] = Some(j);
                children.entry(j).or_default().push(i);
            }
    }
    for i in 0..prs.len() {
        let in_stack = parent[i].is_some() || children.contains_key(&i);
        if !in_stack {
            continue;
        }
        // Depth from the bottom.
        let mut depth = 1;
        let mut cur = i;
        let mut guard = 0;
        while let Some(p) = parent[cur] {
            depth += 1;
            cur = p;
            guard += 1;
            if guard > prs.len() {
                break;
            }
        }
        // Total height: depth of the deepest chain going up from the root `cur`.
        fn height(n: usize, children: &HashMap<usize, Vec<usize>>, guard: usize) -> usize {
            if guard == 0 {
                return 1;
            }
            1 + children
                .get(&n)
                .map(|c| c.iter().map(|&k| height(k, children, guard - 1)).max().unwrap_or(0))
                .unwrap_or(0)
        }
        let total = height(cur, &children, prs.len());
        prs[i].stack = Some((depth, total));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum DiffSide {
    /// The old version (removed lines).
    Left,
    /// The new version (added and context lines).
    Right,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommentAuthor {
    pub login: String,
    #[serde(default)]
    pub avatar_url: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewComment {
    /// `None` for deleted accounts.
    pub author: Option<CommentAuthor>,
    #[serde(default)]
    pub author_association: String,
    pub body: String,
    pub created_at: String,
    #[serde(default)]
    pub url: String,
}

impl ReviewComment {
    pub fn login(&self) -> &str {
        self.author.as_ref().map_or("ghost", |a| a.login.as_str())
    }
}

/// A review comment thread on a line (or range of lines) of a file.
#[derive(Debug, Clone)]
pub struct ReviewThread {
    pub path: String,
    pub side: DiffSide,
    /// Line in the old (`Left`) or new (`Right`) file the thread is attached to.
    pub line: u32,
    /// First line of a multi-line comment.
    pub start_line: Option<u32>,
    pub resolved: bool,
    pub comments: Vec<ReviewComment>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadNode {
    is_resolved: bool,
    is_outdated: bool,
    path: String,
    line: Option<u32>,
    start_line: Option<u32>,
    diff_side: DiffSide,
    comments: Nodes<ReviewComment>,
}

#[derive(Deserialize)]
struct Nodes<T> {
    nodes: Vec<T>,
}

const REVIEW_THREADS_QUERY: &str = r#"
query($owner: String!, $name: String!, $number: Int!, $endCursor: String) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      reviewThreads(first: 100, after: $endCursor) {
        pageInfo { hasNextPage endCursor }
        nodes {
          isResolved isOutdated path line startLine diffSide
          comments(first: 100) {
            nodes { author { login avatarUrl(size: 64) } authorAssociation body createdAt url }
          }
        }
      }
    }
  }
}"#;

/// The PR's review comment threads that are attached to a line of the current diff.
///
/// Outdated threads (whose line is no longer part of the diff) and comments on a
/// whole file are left out, like on GitHub's "Files changed" tab.
pub async fn review_threads(repo: PathBuf, number: u64) -> Result<Vec<ReviewThread>, String> {
    let number = format!("number={number}");
    let query = format!("query={REVIEW_THREADS_QUERY}");
    let json = run(
        &repo,
        "gh",
        &["api", "graphql", "--paginate", "--slurp", "-F", "owner={owner}", "-F", "name={repo}", "-F", &number, "-f", &query],
    )
    .await?;
    parse_review_threads(&json)
}

fn parse_review_threads(json: &str) -> Result<Vec<ReviewThread>, String> {
    #[derive(Deserialize)]
    struct Page {
        data: Data,
    }
    #[derive(Deserialize)]
    struct Data {
        repository: Repository,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Repository {
        pull_request: PullRequestThreads,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct PullRequestThreads {
        review_threads: Nodes<ThreadNode>,
    }
    // `--slurp` wraps the pages in an array.
    let pages: Vec<Page> = serde_json::from_str(json).map_err(|e| format!("Unexpected `gh api graphql` output: {e}"))?;
    Ok(pages
        .into_iter()
        .flat_map(|p| p.data.repository.pull_request.review_threads.nodes)
        .filter(|t| !t.is_outdated && !t.comments.nodes.is_empty())
        .filter_map(|t| {
            Some(ReviewThread {
                path: t.path,
                side: t.diff_side,
                line: t.line?,
                start_line: t.start_line,
                resolved: t.is_resolved,
                comments: t.comments.nodes,
            })
        })
        .collect())
}

/// Fetches, checks out the PR branch and fast-forwards it.
///
/// Returns a short log of what was done.
pub async fn checkout_pr(repo: PathBuf, pr: PullRequest) -> Result<String, String> {
    let remote = remote_name(&repo).await?;
    let mut log = String::new();

    // Fetch first so that a branch which only exists on the remote can be checked out.
    git(&repo, &["fetch", "--prune", &remote]).await?;
    log.push_str(&format!("git fetch --prune {remote}\n"));

    let checkout = if pr.is_cross_repository {
        Err("cross-repository PR".to_string())
    } else {
        git(&repo, &["checkout", &pr.head_ref_name]).await
    };
    match checkout {
        Ok(_) => {
            log.push_str(&format!("git checkout {}\n", pr.head_ref_name));
            git(&repo, &["pull", "--ff-only"]).await?;
            log.push_str("git pull --ff-only\n");
        }
        Err(e) => {
            // Fall back to gh, which also knows how to check out PRs from forks.
            let n = pr.number.to_string();
            run(&repo, "gh", &["pr", "checkout", &n])
                .await
                .map_err(|e2| format!("{e}\n\nFallback `gh pr checkout` also failed:\n{e2}"))?;
            log.push_str(&format!("gh pr checkout {n}\n"));
        }
    }
    git(&repo, &["fetch", "--prune", &remote]).await?;
    log.push_str(&format!("git fetch --prune {remote}\n"));
    Ok(log)
}

/// Computes the merge base between HEAD and the PR's base branch on the remote.
pub async fn merge_base(repo: &Path, base_ref: &str) -> Result<String, String> {
    let remote = remote_name(repo).await?;
    let candidates = [format!("{remote}/{base_ref}"), base_ref.to_string()];
    let mut last_err = String::new();
    for c in &candidates {
        match git(repo, &["merge-base", "HEAD", c]).await {
            Ok(s) => return Ok(s.trim().to_owned()),
            Err(e) => last_err = e,
        }
    }
    Err(format!("Could not compute merge base against {base_ref}:\n{last_err}"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Deleted,
    Modified,
    Renamed,
}

#[derive(Debug, Clone)]
pub struct ChangedFile {
    pub path: String,
    pub old_path: Option<String>,
    pub additions: u32,
    pub deletions: u32,
    pub binary: bool,
    pub kind: ChangeKind,
}

/// Lists changed files between `merge_base` and the working tree.
///
/// Uses `--numstat -z` (machine readable variant of `--stat`) plus `--name-status`
/// to learn about additions/deletions/renames.
pub async fn changed_files(repo: &Path, merge_base: &str) -> Result<Vec<ChangedFile>, String> {
    let numstat = git(repo, &["diff", "--numstat", "-z", "-M", merge_base]).await?;
    let status = git(repo, &["diff", "--name-status", "-z", "-M", merge_base]).await?;

    // name-status -z: "M\0path\0" or "R100\0old\0new\0"
    let mut kinds = std::collections::HashMap::new();
    let mut it = status.split('\0').filter(|s| !s.is_empty());
    while let Some(code) = it.next() {
        let kind = match code.chars().next() {
            Some('A') => ChangeKind::Added,
            Some('D') => ChangeKind::Deleted,
            Some('R') | Some('C') => ChangeKind::Renamed,
            _ => ChangeKind::Modified,
        };
        if kind == ChangeKind::Renamed {
            let _old = it.next();
            if let Some(new) = it.next() {
                kinds.insert(new.to_string(), kind);
            }
        } else if let Some(p) = it.next() {
            kinds.insert(p.to_string(), kind);
        }
    }

    // numstat -z: "add\tdel\tpath\0" or "add\tdel\t\0old\0new\0" for renames.
    let mut files = Vec::new();
    let mut parts = numstat.split('\0');
    while let Some(rec) = parts.next() {
        if rec.is_empty() {
            continue;
        }
        let mut f = rec.splitn(3, '\t');
        let (Some(a), Some(d), Some(p)) = (f.next(), f.next(), f.next()) else { continue };
        let binary = a == "-" && d == "-";
        let (path, old_path) = if p.is_empty() {
            let old = parts.next().unwrap_or_default().to_string();
            let new = parts.next().unwrap_or_default().to_string();
            (new, Some(old))
        } else {
            (p.to_string(), None)
        };
        let kind = kinds.get(&path).cloned().unwrap_or(if old_path.is_some() {
            ChangeKind::Renamed
        } else {
            ChangeKind::Modified
        });
        files.push(ChangedFile {
            path,
            old_path,
            additions: a.parse().unwrap_or(0),
            deletions: d.parse().unwrap_or(0),
            binary,
            kind,
        });
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

/// `git diff <merge base> -- <file>` (with the old path included for renames).
pub async fn file_diff(repo: &Path, merge_base: &str, file: &ChangedFile) -> Result<String, String> {
    let mut args = vec!["diff", "-M", "--no-color", "--no-ext-diff", merge_base, "--"];
    if let Some(old) = &file.old_path {
        args.push(old);
    }
    args.push(&file.path);
    git(repo, &args).await
}

/// `git show <rev>:<path>`, returning `None` for binary content.
pub async fn show_file(repo: &Path, rev: &str, path: &str) -> Result<Option<String>, String> {
    let spec = format!("{rev}:{path}");
    let bytes = run_bytes(repo, "git", &["show", &spec]).await?;
    Ok(text_or_none(bytes))
}

pub fn text_or_none(bytes: Vec<u8>) -> Option<String> {
    if bytes.iter().take(8000).any(|&b| b == 0) {
        return None;
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pr(n: u64, head: &str, base: &str) -> PullRequest {
        PullRequest {
            number: n,
            title: String::new(),
            head_ref_name: head.into(),
            base_ref_name: base.into(),
            author: Author { login: "x".into() },
            is_draft: false,
            created_at: String::new(),
            additions: 0,
            deletions: 0,
            changed_files: 0,
            labels: vec![],
            is_cross_repository: false,
            url: String::new(),
            body: String::new(),
            stack: None,
        }
    }

    #[test]
    fn review_threads() {
        let page = r#"{"data":{"repository":{"pullRequest":{"reviewThreads":{
            "pageInfo":{"hasNextPage":false,"endCursor":null},
            "nodes":[
              {"isResolved":false,"isOutdated":false,"path":"src/a.rs","line":4,"startLine":null,"diffSide":"RIGHT",
               "comments":{"nodes":[{"author":{"login":"octocat","avatarUrl":"https://x/a.png"},"authorAssociation":"OWNER",
                 "body":"Here's a comment!","createdAt":"2026-11-03T10:00:00Z","url":"https://github.com/o/r/pull/7#discussion_r1"}]}},
              {"isResolved":true,"isOutdated":false,"path":"src/a.rs","line":9,"startLine":7,"diffSide":"LEFT",
               "comments":{"nodes":[{"author":null,"authorAssociation":"NONE","body":"old","createdAt":"2026-11-03T10:00:00Z","url":""}]}},
              {"isResolved":false,"isOutdated":true,"path":"src/a.rs","line":null,"startLine":null,"diffSide":"RIGHT",
               "comments":{"nodes":[{"author":{"login":"x","avatarUrl":""},"authorAssociation":"NONE","body":"gone","createdAt":"2026-11-03T10:00:00Z","url":""}]}}
            ]}}}}}"#;
        let threads = parse_review_threads(&format!("[{page},{page}]")).unwrap();
        assert_eq!(threads.len(), 4, "two pages, the outdated thread left out");
        assert_eq!(threads[0].side, DiffSide::Right);
        assert_eq!(threads[0].line, 4);
        assert_eq!(threads[0].comments[0].login(), "octocat");
        assert_eq!((threads[1].side, threads[1].start_line, threads[1].resolved), (DiffSide::Left, Some(7), true));
        assert_eq!(threads[1].comments[0].login(), "ghost");
    }

    #[test]
    fn stacks() {
        let mut prs = vec![pr(3, "b2", "b1"), pr(2, "b1", "main"), pr(1, "t", "main")];
        compute_stacks(&mut prs);
        assert_eq!(prs[0].stack, Some((2, 2)));
        assert_eq!(prs[1].stack, Some((1, 2)));
        assert_eq!(prs[2].stack, None);
    }
}
