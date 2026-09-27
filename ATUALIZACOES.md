# Publicar atualizações pelo Google Drive

O atualizador baixa versões sem pedir login. No Drive, compartilhe o `.exe` e o manifesto como **Qualquer pessoa com o link — Leitor**. O link funciona como uma chave de acesso: quem o receber poderá baixar esses arquivos.

O atualizador não usa assinatura digital. O SHA-256 detecta download incompleto ou corrompido, mas não prova quem publicou o manifesto ou o executável. Compartilhe os links somente com as pessoas para quem você distribui o aplicativo.

## Preparar uma versão

1. Crie no Drive o arquivo de manifesto usando `update-manifest.example.json` como modelo e habilite **Qualquer pessoa com o link — Leitor**. Copie o ID e mantenha esse mesmo arquivo para as próximas versões.
2. Altere a versão em `[workspace.package]` no `Cargo.toml` da raiz.
3. Antes de compilar, defina `P2P_UPDATE_MANIFEST_URL` como o link estável de download do manifesto no Drive. Use o formato direto `https://drive.google.com/uc?export=download&id=ID_DO_ARQUIVO`.
4. Compile o cliente em modo release, por exemplo `cargo build --release -p p2p-client`. O executável fica em `target/release/p2p-client.exe`.
5. Envie esse executável ao Drive e obtenha o ID do arquivo. Monte seu link direto no mesmo formato `uc?export=download&id=...`.
6. Gere os dados do manifesto no PowerShell:

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

Troque `version` pela versão compilada e `download_url` pelo link real do executável. Para publicar a primeira versão, atualize o manifesto no Drive. Nas seguintes, use **Organizar > Gerenciar versões > Fazer upload de nova versão** no arquivo do manifesto existente, para preservar o ID e o link estável. O leitor do manifesto aceita UTF-8 com ou sem BOM.

Para compilar com o endereço do manifesto embutido, defina a variável antes do build:

```powershell
$env:P2P_UPDATE_MANIFEST_URL = "https://drive.google.com/uc?export=download&id=ID_DO_MANIFESTO"
cargo build --release -p p2p-client
Remove-Item Env:P2P_UPDATE_MANIFEST_URL
```

O app exige HTTPS, segue redirecionamentos e confere o tamanho, SHA-256 e cabeçalho `MZ` do executável baixado. Se o Drive responder com uma página HTML de login, cota ou confirmação, o app interrompe o processo sem substituir o executável. Ele não usa a API autenticada do Google Drive.

As versões já distribuídas sem atualizador precisam receber uma cópia nova do `.exe` manualmente uma vez. Depois disso, o app oferece as atualizações na página **Configurações > Atualizações**.
