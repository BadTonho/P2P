# Publicar atualizações pelo Google Drive

O aplicativo procura `update-manifest.json` na [pasta de atualizações do projeto](https://drive.google.com/drive/folders/1byA2jwpqhjICOeZWeI1i_Lfn2KDqr4fh?usp=sharing). Compartilhe a pasta e os arquivos como **Qualquer pessoa com o link — Leitor** para que os amigos baixem sem fazer login.

## Versão 1.0.0 preparada localmente

O pacote atual é `target/release/p2p-client.exe`. O `update-manifest.json` gerado junto é um **rascunho**: seu campo `download_url` contém `ID_AINDA_NAO_PUBLICADO`. Não envie esse manifesto ao Drive. Primeiro envie o executável, copie o ID do arquivo e só então troque o placeholder pelo ID real.

Para publicar:

1. Envie `target/release/p2p-client.exe` à pasta do Drive. Configure o arquivo como **Qualquer pessoa com o link — Leitor** e copie o ID do executável.
2. Confirme que o executável no Drive corresponde aos bytes locais. Gere o manifesto a partir do arquivo local:

```powershell
$exe = (Resolve-Path "target/release/p2p-client.exe").Path
$downloadId = "ID_REAL_DO_EXECUTAVEL"
$manifest = [ordered]@{
    version = "1.0.0"
    download_url = "https://drive.google.com/uc?export=download&id=$downloadId"
    size_bytes = (Get-Item -LiteralPath $exe).Length
    sha256 = (Get-FileHash -LiteralPath $exe -Algorithm SHA256).Hash.ToLowerInvariant()
}
$manifest | ConvertTo-Json | Set-Content -LiteralPath "update-manifest.json" -Encoding utf8
```

3. Envie o `update-manifest.json` atualizado à mesma pasta, também como **Qualquer pessoa com o link — Leitor**. Mantenha apenas um arquivo com esse nome na pasta.
4. Antes de compartilhar, teste o link do executável em uma janela privada do navegador e confirme que não exige login. Depois, em **Configurações > Atualizações**, clique em **Verificar atualizações**.

Nas versões seguintes, compile uma versão maior em `[workspace.package]`, envie os novos bytes para o Drive e atualize o manifesto com a nova versão, tamanho, SHA-256 e o ID do executável. Se substituir o arquivo existente no Drive, mantenha o mesmo ID; se enviar um arquivo novo, atualize `download_url` para o novo ID.

## Chave da Google Drive API

A compilação distribuída precisa incluir uma chave restrita à Google Drive API para localizar o manifesto público na pasta. A chave fica embutida no `.exe` e pode ser extraída; não a trate como senha. Configure restrição de API e limites de uso no Google Cloud. A chave não concede acesso aos arquivos privados da conta.

Defina `P2P_UPDATE_DRIVE_API_KEY` somente no ambiente temporário do build. O repositório tem um `Chave.md` local ignorado pelo Git com uma única chave identificável; o valor não deve ser impresso, registrado ou commitado. Remova a variável de ambiente após a compilação. Sem a chave, o atualizador não consegue procurar o manifesto na pasta.

## Limites e segurança

O download exige HTTPS e valida tamanho, SHA-256 e assinatura PE (`MZ`). O aplicador de atualização guarda uma cópia de recuperação e restaura a versão anterior se a substituição ou inicialização da nova versão falhar. Não há assinatura digital: SHA-256 detecta corrupção, mas não prova quem publicou o manifesto ou o executável.

As versões distribuídas anteriormente sem o atualizador precisam receber a versão 1.0.0 manualmente uma vez. Depois, podem usar a opção em **Configurações > Atualizações**. O download e a aplicação são iniciados pelo usuário e ficam bloqueados durante uma sala ativa.
