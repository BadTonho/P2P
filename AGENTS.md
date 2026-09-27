# Instruções do projeto

## Atualizações pelo Google Drive

- Preserve o atualizador pelo Google Drive em mudanças futuras. Ele deve localizar `update-manifest.json` na pasta pública configurada e aceitar atualizações somente após validar tamanho, SHA-256 e executável Windows.
- Não substitua a busca na pasta por um link direto de manifesto sem combinar isso com o usuário. O ID padrão da pasta está em `crates/p2p-client/build.rs`; mantenha-o apontando para a pasta de atualizações do projeto.
- O build usa `P2P_UPDATE_DRIVE_API_KEY` para incluir uma chave restrita à Google Drive API no executável. Nunca escreva o valor da chave em arquivos rastreados, logs, documentação ou mensagens. Use a variável de ambiente apenas durante a compilação e remova-a ao terminar.
- Se houver um arquivo local `Chave.md` ignorado pelo Git, ele pode ser usado como fonte local da chave para a compilação. Leia seu conteúdo sem exibi-lo, copie apenas o valor necessário para a variável de ambiente temporária e não altere nem rastreie esse arquivo. Se o formato não for claro, peça confirmação antes de usá-lo.
- Não distribua um build de release sem a chave configurada: ele ficaria sem a verificação automática do Drive. Se a chave não estiver disponível no ambiente ou no contexto autorizado da tarefa, explique que ela é necessária antes de gerar o executável de distribuição.
- Mantenha `ATUALIZACOES.md` alinhado com a implementação e com o processo de publicação.

## Preparar uma versão para enviar ao Drive

Quando o usuário pedir para preparar o instalador, executável ou versão para subir no Google Drive, gere **os dois artefatos**: o executável Windows e `update-manifest.json`. Neste projeto, a distribuição combinada até agora é um `.exe` avulso, sem instalador separado; não crie um instalador MSI ou similar sem pedido específico.

1. Use a versão definida em `[workspace.package]` no `Cargo.toml` da raiz e compile `p2p-client` em `release`, com a chave do Drive disponível somente como variável de ambiente.
2. Prepare o executável final `p2p-client.exe` e gere o manifesto a partir desse arquivo exato. O campo `version` deve corresponder à versão compilada; `size_bytes` e `sha256` devem ser calculados sobre os bytes finais do executável.
3. O manifesto deve se chamar exatamente `update-manifest.json` e conter `version`, `download_url`, `size_bytes` e `sha256`, seguindo `update-manifest.example.json`.
4. `download_url` precisa usar o link direto `https://drive.google.com/uc?export=download&id=ID_REAL_DO_EXECUTAVEL`. Se o ID do executável no Drive já for conhecido, use-o. Para versões futuras, prefira enviar uma nova versão do mesmo arquivo no Drive para preservar o ID.
5. Se o ID do executável ainda não existir porque o usuário vai subir o arquivo depois, gere o executável e um manifesto claramente marcado como **rascunho** com placeholder no `download_url`; explique que o manifesto só estará pronto para publicar depois que o usuário enviar o executável e fornecer o ID/link do Drive. Não apresente um placeholder como manifesto final.
6. Entregue os caminhos dos dois artefatos e os passos de upload. Deixe claro que o manifesto e o executável precisam ser enviados à pasta configurada e que deve haver apenas um `update-manifest.json` nela.

Consulte `ATUALIZACOES.md` antes de alterar o formato, a publicação ou o mecanismo de atualização.
