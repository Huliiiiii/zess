use std::collections::HashMap;

use zess_git::{BlobRead, Execution, GitQueries, LineStats, RepositoryLocation};
use zess_platform::{NativeFileSystem, NativeWatchBackend, SystemProcessRunner};

use super::*;

struct Queries;

impl GitQueries for Queries {
    fn discover(&self, _: &Path) -> anyhow::Result<Option<RepositoryLocation>> {
        Ok(None)
    }

    fn worktree_counts(
        &self,
        _: &Path,
        _: &dyn Execution,
    ) -> anyhow::Result<HashMap<String, Option<LineStats>>> {
        Ok(HashMap::from([(
            "note.txt".into(),
            Some(LineStats {
                additions: 91,
                deletions: 17,
            }),
        )]))
    }

    fn commit_counts(
        &self,
        _: &Path,
        _: &str,
        _: &dyn Execution,
    ) -> anyhow::Result<HashMap<String, Option<LineStats>>> {
        anyhow::bail!("no commit query in this scenario")
    }

    fn blob(&self, _: &Path, _: &str, _: &str, _: &dyn Execution) -> anyhow::Result<BlobRead> {
        anyhow::bail!("no blob query in this scenario")
    }

    fn commit_parent(
        &self,
        _: &Path,
        _: &str,
        _: &dyn Execution,
    ) -> anyhow::Result<Option<String>> {
        anyhow::bail!("no parent query in this scenario")
    }
}

#[test]
fn service_queries_use_the_injected_git_backend() {
    let fixture = Fixture::new();
    git(&fixture.0, &["init", "--quiet"]);
    fs::write(fixture.0.join("note.txt"), "baseline\n").unwrap();
    git(&fixture.0, &["add", "."]);
    git(&fixture.0, &["commit", "--quiet", "-m", "baseline"]);
    fs::write(fixture.0.join("note.txt"), "changed\n").unwrap();
    let mut services = NativeServices::with_git_queries(
        fixture.0.clone(),
        Arc::new(NativeFileSystem),
        Arc::new(SystemProcessRunner),
        Arc::new(NativeWatchBackend),
        Arc::new(Queries),
    )
    .unwrap();
    let decorations = services.call("git.decorations", json!({}));
    assert_eq!(decorations["entries"], json!([]), "{decorations}");
    let status = services.call("git.status", json!({}));
    assert_eq!(status["entries"][0]["path"], "note.txt", "{status}");
    assert_eq!(
        status["entries"][0]["line_stats"],
        json!({"additions":91,"deletions":17}),
        "{status}"
    );
}
