# Instruções do projeto

## Atualizações pelo GitHub Releases

- O atualizador consulta o último GitHub Release público em `https://api.github.com/repos/BadTonho/P2P/releases/latest`. Não adicione tokens, chaves de API ou credenciais ao executável.
- Cada release estável precisa ter uma tag numérica `vMAJOR.MINOR.PATCH` e exatamente um asset chamado `p2p-client.exe`. A versão da tag deve corresponder à versão do workspace no `Cargo.toml` da raiz.
- O app usa a tag, o tamanho, o SHA-256 (`digest`) e o link do asset retornados pela API pública do GitHub. Não é necessário criar nem publicar um manifesto separado.
- Cada release também pode incluir `P2P-Voz-e-tela-Setup.exe`; preserve o asset `p2p-client.exe` com esse nome exato para o atualizador.
- Preserve o download iniciado pelo usuário, a validação HTTPS/tamanho/SHA-256/assinatura PE e o aplicador com backup e rollback. Não registre dados privados ou tokens.
- Preserve as preferências locais em `%LOCALAPPDATA%\P2P-Voz-e-tela\settings.json` durante atualizações. Não salve códigos de sala nem credenciais temporárias.
- Mantenha `ATUALIZACOES.md` alinhado à implementação e ao processo de publicação.
- A primeira versão pública será 1.0.0 com o atualizador do GitHub. Não reintroduza dependência do Google Drive nem instruções de migração do Drive sem pedido explícito.

## Preparar um release

Quando o usuário pedir para preparar uma versão para o GitHub, gere `target/release/p2p-client.exe` e `target/installer/P2P-Voz-e-tela-Setup.exe`, confira as versões e os hashes e explique como anexar ambos a um GitHub Release público, preservando os nomes exatos dos assets. Use `scripts/build-installer.ps1` e Inno Setup 6. Não gere `update-manifest.json`; a API do GitHub fornece os metadados usados pelo app.

Antes de preparar uma versão, consulte `ATUALIZACOES.md`. Não publique nem crie releases externos sem autorização explícita.
