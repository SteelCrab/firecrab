use super::*;
use firecrab_api_types::{CodeBranchPage, CodeRepositoryChoice, CodeRepositoryPage};

const SIZE: usize = 20;
fn valid(account: &str, owner: &str, repository: &str, branch: &str) -> bool {
    CodeGithubRepository {
        account: account.into(),
        owner: owner.into(),
        repository: repository.into(),
        branch: branch.into(),
    }
    .validate()
    .is_empty()
}
fn query(page: u32) -> Result<Vec<(&'static str, String)>, Failure> {
    if !(1..=1000).contains(&page) {
        return Err(Failure::InvalidResponse);
    }
    Ok(vec![
        ("per_page", SIZE.to_string()),
        ("page", page.to_string()),
    ])
}
impl Github {
    /// List bounded sanitized choices at the fixed GitHub origin.
    pub(crate) async fn repositories(
        &self,
        login: &str,
        auth: Option<&SavedAuth>,
        page: u32,
    ) -> Result<CodeRepositoryPage, Failure> {
        let mut query = query(page)?;
        query.push(("sort", "full_name".into()));
        query.push(("direction", "asc".into()));
        let token = self.identity(login, auth).await?;
        #[derive(Deserialize)]
        struct Owner {
            login: String,
        }
        #[derive(Deserialize)]
        struct Repo {
            owner: Owner,
            name: String,
            full_name: String,
            default_branch: String,
            private: bool,
            disabled: bool,
        }
        let repos: Vec<Repo> = self.get_query(&["user", "repos"], token, &query).await?;
        if repos.len() > SIZE {
            return Err(Failure::InvalidResponse);
        }
        let next_page = (repos.len() == SIZE && page < 1000).then_some(page + 1);
        let mut repositories = Vec::new();
        for repo in repos {
            if !valid(login, &repo.owner.login, &repo.name, &repo.default_branch)
                || !repo
                    .full_name
                    .eq_ignore_ascii_case(&format!("{}/{}", repo.owner.login, repo.name))
            {
                return Err(Failure::InvalidResponse);
            }
            if !repo.disabled {
                repositories.push(CodeRepositoryChoice {
                    owner: repo.owner.login,
                    repository: repo.name,
                    default_branch: repo.default_branch,
                    private: repo.private,
                });
            }
        }
        Ok(CodeRepositoryPage {
            account: login.into(),
            repositories,
            next_page,
        })
    }

    /// Discover exact branches without following provider links or returning commit payloads.
    pub(crate) async fn branches(
        &self,
        spec: &CodeGithubRepository,
        auth: Option<&SavedAuth>,
        page: u32,
    ) -> Result<CodeBranchPage, Failure> {
        let query = query(page)?;
        if !spec.validate().is_empty() {
            return Err(Failure::InvalidResponse);
        }
        let token = self.identity(&spec.account, auth).await?;
        #[derive(Deserialize)]
        struct Branch {
            name: String,
        }
        let branches: Vec<Branch> = self
            .get_query(
                &["repos", &spec.owner, &spec.repository, "branches"],
                token,
                &query,
            )
            .await?;
        if branches.len() > SIZE
            || branches
                .iter()
                .any(|b| !valid(&spec.account, &spec.owner, &spec.repository, &b.name))
        {
            return Err(Failure::InvalidResponse);
        }
        let next_page = (branches.len() == SIZE && page < 1000).then_some(page + 1);
        Ok(CodeBranchPage {
            account: spec.account.clone(),
            owner: spec.owner.clone(),
            repository: spec.repository.clone(),
            branches: branches.into_iter().map(|b| b.name).collect(),
            next_page,
        })
    }
}
