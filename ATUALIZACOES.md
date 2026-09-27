# Publicar atualizações pelo Google Drive

O atualizador já aponta para a [pasta de atualizações do projeto](https://drive.google.com/drive/folders/1byA2jwpqhjICOeZWeI1i_Lfn2KDqr4fh?usp=sharing) e procura nela um arquivo chamado `update-manifest.json`. A pasta está pronta para receber esse manifesto e os executáveis.

Compartilhe a pasta e os arquivos como **Qualquer pessoa com o link — Leitor**. Os amigos não precisam fazer login no Google. O aplicativo usa uma chave da Google Drive API apenas para localizar o manifesto público dentro da pasta; essa chave não dá acesso aos arquivos privados da sua conta.

Para habilitar a busca automática, crie um projeto no Google Cloud, habilite a Google Drive API e [crie uma API key](https://developers.google.com/workspace/guides/create-credentials). O Google documenta [como listar arquivos de uma pasta pública com uma API key](https://developers.google.com/workspace/drive/api/guides/search-files). Restrinja a chave para permitir somente a Google Drive API. Como a chave será embutida no `.exe`, ela poderá ser extraída; não a trate como senha e configure limites de uso no Google Cloud.

O atualizador não usa assinatura digital. O SHA-256 detecta download incompleto ou corrompido, mas não prova quem publicou o manifesto ou o executável. Compartilhe os links somente com as pessoas para quem você distribui o aplicativo.

## Preparar uma versão

1. Altere a versão em `[workspace.package]` no `Cargo.toml` da raiz.
2. Compile uma primeira vez para gerar o executável, por exemplo `cargo build --release -p p2p-client`. O arquivo fica em `target/release/p2p-client.exe`.
3. Envie esse executável à pasta do projeto e copie o ID do arquivo. Monte o link de download direto no formato `https://drive.google.com/uc?export=download&id=ID_DO_EXECUTAVEL`.
4. Gere os dados do manifesto no PowerShell:

```powershell
$exe = (Resolve-Path "target/release/p2p-client.exe").Path
$hash = (Get-FileHash -LiteralPath $exe -Algorithm SHA256).Hash.ToLowerInvariant()
$size = (Get-Item -LiteralPath $exe).Length
$manifest = [ordered]@{
    version = "0.1.1"
    download_url = "https://drive.google.com/uc?export=download&id=ID_DO_EXECUTAVEL"
    size_bytes = $size
    sha256 = $hash
}
$manifest | ConvertTo-Json | Set-Content -LiteralPath "update-manifest.json" -Encoding utf8
```

Troque `version` pela versão compilada e `download_url` pelo link real do executável. Envie `update-manifest.json` para a pasta configurada e deixe apenas um arquivo com esse nome. Nas versões seguintes, substitua o manifesto por uma versão nova com o mesmo nome. O app encontra o arquivo pelo nome e lê seu ID pela Drive API. O leitor do manifesto aceita UTF-8 com ou sem BOM.

Antes de compilar o aplicativo que será distribuído, forneça sua API key no ambiente:

```powershell
$env:P2P_UPDATE_DRIVE_API_KEY = "SUA_CHAVE_DA_GOOGLE_DRIVE_API"
cargo build --release -p p2p-client
Remove-Item Env:P2P_UPDATE_DRIVE_API_KEY
```

O ID da pasta já está configurado no projeto. Pode ser substituído em `P2P_UPDATE_DRIVE_FOLDER_ID` antes da compilação. Sem a API key, o app mostra que a integração não está habilitada e não tenta verificar atualizações. A chave é enviada pelo cabeçalho HTTPS `x-goog-api-key`; ela fica embutida no executável distribuído e pode ser extraída dele.

O app exige HTTPS, segue redirecionamentos e confere o tamanho, SHA-256 e cabeçalho `MZ` do executável baixado. Se o Drive responder com uma página HTML de login, cota ou confirmação, o app interrompe o processo sem substituir o executável. A busca usa a API key para arquivos públicos e não pede login aos amigos.

As versões já distribuídas sem atualizador precisam receber uma cópia nova do `.exe` manualmente uma vez. Depois disso, o app oferece as atualizações na página **Configurações > Atualizações**.
