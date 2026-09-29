use super::*;

pub(super) fn new_http_client() -> Result<Client, String> {
    Client::builder()
        .user_agent(concat!("P2P-Voz-e-tela/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(600))
        .redirect(Policy::limited(10))
        .build()
        .map_err(|_| "Não foi possível preparar a conexão HTTPS.".to_owned())
}

pub(super) fn validate_https_url(value: &str) -> Result<(), String> {
    let url = reqwest::Url::parse(value)
        .map_err(|_| "O link de atualização não é uma URL válida.".to_owned())?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("Os links de atualização precisam usar HTTPS.".to_owned());
    }
    Ok(())
}

pub(super) fn check_for_update() -> Result<Option<UpdateManifest>, String> {
    let client = new_http_client()?;
    let response = client
        .get(GITHUB_RELEASE_API_URL)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .map_err(|error| sanitized_request_error(&error))?;
    if response.status().as_u16() == 404 {
        return Err(
            "Ainda não há um GitHub Release público com atualização para este aplicativo."
                .to_owned(),
        );
    }
    if response.status().as_u16() == 403 {
        return Err("O GitHub limitou ou recusou a consulta pública de releases. Aguarde alguns minutos e tente novamente.".to_owned());
    }
    let response = validate_response(response, RELEASE_MAX_BYTES, "informações do release")?;
    if is_html(&response) {
        return Err("O GitHub respondeu com HTML em vez dos dados do release.".to_owned());
    }
    let mut bytes = Vec::new();
    response
        .take(RELEASE_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Não foi possível ler as informações do GitHub Release.".to_owned())?;
    if bytes.len() as u64 > RELEASE_MAX_BYTES {
        return Err("As informações do GitHub Release excedem o limite de tamanho.".to_owned());
    }
    let manifest = parse_github_release(&bytes)?;

    match compare_versions(&manifest.version, env!("CARGO_PKG_VERSION"))? {
        std::cmp::Ordering::Greater => Ok(Some(manifest)),
        std::cmp::Ordering::Equal | std::cmp::Ordering::Less => Ok(None),
    }
}

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    #[serde(default)]
    assets: Vec<GithubReleaseAsset>,
}

#[derive(Debug, Deserialize)]
struct GithubReleaseAsset {
    name: String,
    browser_download_url: String,
    size: u64,
    digest: Option<String>,
}

pub(super) fn parse_github_release(bytes: &[u8]) -> Result<UpdateManifest, String> {
    let release: GithubRelease = serde_json::from_slice(bytes)
        .map_err(|_| "O GitHub não retornou dados válidos de release.".to_owned())?;
    let version = release
        .tag_name
        .strip_prefix('v')
        .unwrap_or(&release.tag_name)
        .to_owned();
    parse_version(&version)?;

    let mut assets = release
        .assets
        .into_iter()
        .filter(|asset| asset.name == RELEASE_ASSET_NAME);
    let Some(asset) = assets.next() else {
        return Err(format!(
            "O último release não contém o arquivo {RELEASE_ASSET_NAME}."
        ));
    };
    if assets.next().is_some() {
        return Err(format!(
            "O último release contém mais de um arquivo {RELEASE_ASSET_NAME}."
        ));
    }
    let sha256 = asset
        .digest
        .as_deref()
        .and_then(|digest| digest.strip_prefix("sha256:"))
        .ok_or_else(|| "O GitHub não informou o SHA-256 do executável publicado.".to_owned())?
        .to_owned();
    let manifest = UpdateManifest {
        version,
        download_url: asset.browser_download_url,
        size_bytes: asset.size,
        sha256,
    };
    validate_manifest(&manifest)?;
    validate_release_asset_url(&manifest.download_url)?;
    Ok(manifest)
}

fn validate_release_asset_url(value: &str) -> Result<(), String> {
    let url = reqwest::Url::parse(value)
        .map_err(|_| "O link do executável no GitHub Release é inválido.".to_owned())?;
    let expected_prefix = format!("/{GITHUB_OWNER}/{GITHUB_REPOSITORY}/releases/download/");
    if url.scheme() != "https"
        || url.host_str() != Some("github.com")
        || !url.username().is_empty()
        || url.password().is_some()
        || !url.path().starts_with(&expected_prefix)
        || !url.path().ends_with(&format!("/{RELEASE_ASSET_NAME}"))
    {
        return Err(
            "O link do executável não pertence aos releases HTTPS deste repositório.".to_owned(),
        );
    }
    Ok(())
}

pub(super) fn validate_response(
    response: Response,
    max_bytes: u64,
    description: &str,
) -> Result<Response, String> {
    let status = response.status();
    if !status.is_success() {
        return Err(format!(
            "O servidor de atualizações respondeu com HTTP {status}."
        ));
    }
    if response.url().scheme() != "https" {
        return Err(
            "O redirecionamento do servidor de atualizações deixou de usar HTTPS.".to_owned(),
        );
    }
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes)
    {
        return Err(format!("O {description} excede o limite de tamanho."));
    }
    Ok(response)
}

pub(super) fn is_html(response: &Response) -> bool {
    if response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.to_ascii_lowercase().contains("text/html"))
    {
        return true;
    }
    false
}

pub(super) fn looks_like_html(bytes: &[u8]) -> bool {
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    let prefix = String::from_utf8_lossy(bytes);
    let prefix = prefix.trim_start().to_ascii_lowercase();
    prefix.starts_with("<!doctype html") || prefix.starts_with("<html")
}

pub(super) fn validate_manifest(manifest: &UpdateManifest) -> Result<(), String> {
    parse_version(&manifest.version)?;
    validate_https_url(&manifest.download_url)?;
    if manifest.size_bytes == 0 || manifest.size_bytes > UPDATE_MAX_BYTES {
        return Err("O tamanho informado pelo GitHub está fora do limite aceito.".to_owned());
    }
    if manifest.sha256.len() != 64 || !manifest.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("O SHA-256 do release precisa ter 64 caracteres hexadecimais.".to_owned());
    }
    Ok(())
}

pub(super) fn parse_version(value: &str) -> Result<[u64; 3], String> {
    let mut parts = value.split('.');
    let parsed = [parts.next(), parts.next(), parts.next()];
    if parts.next().is_some() {
        return Err("A tag do release deve conter uma versão MAJOR.MINOR.PATCH, opcionalmente iniciada por v.".to_owned());
    }
    let mut version = [0; 3];
    for (index, part) in parsed.into_iter().enumerate() {
        let Some(part) = part else {
            return Err("A tag do release deve conter uma versão MAJOR.MINOR.PATCH, opcionalmente iniciada por v.".to_owned());
        };
        if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err("A tag do release deve conter uma versão MAJOR.MINOR.PATCH, opcionalmente iniciada por v.".to_owned());
        }
        version[index] = part
            .parse()
            .map_err(|_| "A versão informada é grande demais.".to_owned())?;
    }
    Ok(version)
}

pub(super) fn compare_versions(left: &str, right: &str) -> Result<std::cmp::Ordering, String> {
    Ok(parse_version(left)?.cmp(&parse_version(right)?))
}

pub(super) fn sanitized_request_error(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        "A conexão com o servidor de atualizações expirou.".to_owned()
    } else if error.is_connect() {
        "Não foi possível conectar ao servidor de atualizações.".to_owned()
    } else {
        "Falha ao receber dados do servidor de atualizações.".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::test_support::manifest;

    #[test]
    fn compares_numeric_versions_by_component() {
        assert!(compare_versions("0.10.0", "0.9.9").unwrap().is_gt());
        assert!(compare_versions("1.0.0", "1.0.0").unwrap().is_eq());
        assert!(compare_versions("0.1.0", "0.2.0").unwrap().is_lt());
    }

    #[test]
    fn accepts_only_https_links_without_embedded_credentials() {
        assert!(validate_https_url("https://github.com/BadTonho/P2P/releases/latest").is_ok());
        assert!(validate_https_url("http://github.com/BadTonho/P2P/releases/latest").is_err());
        assert!(
            validate_https_url("https://user:pass@github.com/BadTonho/P2P/releases/latest")
                .is_err()
        );
    }

    #[test]
    fn parses_github_release_asset_and_uses_its_digest() {
        let body = br#"{
            "tag_name":"v1.0.1",
            "assets":[{
                "name":"p2p-client.exe",
                "browser_download_url":"https://github.com/BadTonho/P2P/releases/download/v1.0.1/p2p-client.exe",
                "size":123,
                "digest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            }]
        }"#;
        assert_eq!(parse_github_release(body).unwrap(), manifest());
        assert!(looks_like_html(b" <!doctype html><html>GitHub error"));
        assert!(parse_github_release(b"<html>login</html>").is_err());
    }

    #[test]
    fn rejects_invalid_release_fields() {
        let mut value = manifest();
        assert!(validate_manifest(&value).is_ok());
        value.size_bytes = 0;
        assert!(validate_manifest(&value).is_err());
        value.size_bytes = 123;
        value.sha256 = "not-a-hash".to_owned();
        assert!(validate_manifest(&value).is_err());
        value.sha256 = "a".repeat(64);
        value.version = "0.1".to_owned();
        assert!(validate_manifest(&value).is_err());
    }
}
