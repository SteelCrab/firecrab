//! Account connection instructions shared by the overview and credential forms.
use crate::model::escape;

fn tr<'a>(ko: bool, en: &'a str, korean: &'a str) -> &'a str {
    if ko { korean } else { en }
}

fn title(provider: &str, ko: bool) -> String {
    format!(
        "{provider} {}",
        tr(ko, "connection steps", "계정 연동 방법")
    )
}

fn steps(provider: &str, ko: bool) -> String {
    let (instructions, note, link_label, link_url, reference) = match provider {
        "Firecrab" => (
            [
                tr(
                    ko,
                    "Sign in to the Firecrab Console using the link below.",
                    "아래 Firecrab 콘솔 링크에서 로그인한다.",
                ),
                tr(
                    ko,
                    "Open Account integration and issue an account integration token.",
                    "Account integration에서 계정 연동 토큰을 발급한다.",
                ),
                tr(
                    ko,
                    "Paste the token starting with fca_ into Account integration token.",
                    "fca_로 시작하는 토큰을 계정 연동 토큰에 붙여 넣는다.",
                ),
                tr(
                    ko,
                    "Select Save, then check the linked account ID and email.",
                    "저장을 누른 뒤 연결된 계정 ID와 이메일을 확인한다.",
                ),
            ],
            "",
            "console.firecrab.dev",
            "https://console.firecrab.dev/#account",
            "",
        ),
        "GitHub" => (
            [
                tr(
                    ko,
                    "Open GitHub token settings and create a personal access token (classic).",
                    "GitHub 토큰 설정에서 Personal access token (classic)을 발급한다.",
                ),
                tr(
                    ko,
                    "Enable the read:packages permission when issuing the token.",
                    "토큰 발급 시 read:packages 권한을 선택한다.",
                ),
                tr(
                    ko,
                    "Paste the issued token into Token and select Save.",
                    "발급한 토큰을 토큰 입력란에 붙여 넣고 저장을 누른다.",
                ),
                tr(
                    ko,
                    "Open Pipe → Registry and check that ghcr.io is listed.",
                    "Pipe → Registry에서 ghcr.io가 표시되는지 확인한다.",
                ),
            ],
            tr(
                ko,
                "If your organization requires SSO, authorize this token for the organization.",
                "조직에서 SSO를 사용하면 해당 조직에 토큰 사용을 승인한다.",
            ),
            tr(ko, "GitHub token settings", "GitHub 토큰 설정 열기"),
            "https://github.com/settings/tokens",
            "https://docs.github.com/en/packages/working-with-a-github-packages-registry/working-with-the-container-registry#authenticating-to-the-container-registry",
        ),
        "AWS" => (
            [
                tr(
                    ko,
                    "Prepare access keys for an IAM user or role with ECR read permissions.",
                    "ECR 읽기 권한이 있는 IAM 사용자 또는 역할의 액세스 키를 준비한다.",
                ),
                tr(
                    ko,
                    "Enter Access key ID and Secret access key.",
                    "액세스 키 ID와 보안 액세스 키를 입력한다.",
                ),
                tr(
                    ko,
                    "Enter the ECR region, such as ap-northeast-2. For temporary credentials, also enter Session token.",
                    "ECR 리전을 입력한다. 예: ap-northeast-2. 임시 키는 세션 토큰도 입력한다.",
                ),
                tr(
                    ko,
                    "Select Save, then check the account's ECR registry in Pipe → Registry.",
                    "저장을 누른 뒤 Pipe → Registry에서 계정의 ECR 저장소를 확인한다.",
                ),
            ],
            tr(
                ko,
                "Account verification needs ecr:GetAuthorizationToken. Repository listing needs ecr:DescribeRepositories, and image downloads need pull permissions.",
                "계정 확인에는 ecr:GetAuthorizationToken, 저장소 목록에는 ecr:DescribeRepositories 권한이 필요하다. 이미지 다운로드에는 가져오기 권한도 필요하다.",
            ),
            tr(ko, "AWS access key guide", "AWS 액세스 키 발급 안내"),
            "https://docs.aws.amazon.com/IAM/latest/UserGuide/access-key-self-managed.html",
            "https://docs.aws.amazon.com/AmazonECR/latest/userguide/registry_auth.html",
        ),
        _ => return String::new(),
    };
    let mut out = String::from("<ol class=\"auth-setup-steps\">");
    for instruction in instructions {
        out.push_str(&format!("<li>{}</li>", escape(instruction)));
    }
    out.push_str("</ol>");
    if !note.is_empty() {
        out.push_str(&format!(
            "<p class=\"auth-setup-note\">{}</p>",
            escape(note)
        ));
    }
    out.push_str(&format!(
        "<div class=\"auth-setup-links\"><a href=\"{}\" target=\"_blank\" rel=\"noopener noreferrer\">{}</a>",
        escape(link_url), escape(link_label)
    ));
    if !reference.is_empty() {
        out.push_str(&format!(
            "<a href=\"{}\" target=\"_blank\" rel=\"noopener noreferrer\">{}</a>",
            escape(reference),
            tr(ko, "Authentication guide", "인증 권한 안내")
        ));
    }
    out.push_str("</div>");
    out
}

pub fn form_guide(path: &str) -> String {
    let provider = match path {
        "/api/pipe/auth/firecrab" => "Firecrab",
        "/api/pipe/auth/github" => "GitHub",
        "/api/pipe/auth/aws" => "AWS",
        _ => return String::new(),
    };
    let ko = crate::i18n::korean();
    let heading = title(provider, ko);
    format!(
        "<section class=\"auth-setup wasm-wide\" aria-label=\"{}\"><h3>{}</h3>{}</section>",
        escape(&heading),
        escape(&heading),
        steps(provider, ko)
    )
}

pub fn overview(ko: bool) -> String {
    let mut out = format!(
        "<section class=\"auth-setup-overview\" aria-label=\"{}\"><h2>{}</h2><div class=\"auth-setup-grid\">",
        tr(ko, "Account connection guide", "계정 연동 안내"),
        tr(ko, "How to connect each account", "계정별 연동 방법")
    );
    for provider in ["Firecrab", "GitHub", "AWS"] {
        out.push_str(&format!(
            "<details class=\"auth-setup\"><summary>{}</summary>{}</details>",
            escape(&title(provider, ko)),
            steps(provider, ko)
        ));
    }
    out.push_str("</div></section>");
    out
}
