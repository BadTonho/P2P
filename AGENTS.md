# Instruções do projeto

## Atualizações pelo GitHub Releases

- O atualizador consulta o último GitHub Release público em `https://api.github.com/repos/BadTonho/P2P/releases/latest`. Não adicione tokens, chaves de API ou credenciais ao executável.
- Cada release estável precisa ter uma tag numérica `vMAJOR.MINOR.PATCH` e exatamente um asset chamado `p2p-client.exe`. A versão da tag deve corresponder à versão do workspace no `Cargo.toml` da raiz.
- O app usa a tag, o tamanho, o SHA-256 (`digest`) e o link do asset retornados pela API pública do GitHub. Não é necessário criar nem publicar um manifesto separado.
- Preserve o download iniciado pelo usuário, a validação HTTPS/tamanho/SHA-256/assinatura PE e o aplicador com backup e rollback. Não registre dados privados ou tokens.
- Mantenha `ATUALIZACOES.md` alinhado à implementação e ao processo de publicação.
- O atualizador anterior da versão 1.0.0 consultava o Google Drive. Uma versão GitHub Release precisa ser instalada manualmente uma vez por quem ainda estiver nessa versão; não remova instruções sobre essa migração até ela ser concluída.

## Preparar um release

Quando o usuário pedir para preparar uma versão para o GitHub, gere o executável Windows `target/release/p2p-client.exe`, confira sua versão, tamanho e SHA-256 e explique como anexá-lo a um GitHub Release público com o nome exato `p2p-client.exe`. Não gere `update-manifest.json`; a API do GitHub fornece os metadados usados pelo app. Não crie instalador MSI ou similar sem pedido específico.

Antes de preparar uma versão, consulte `ATUALIZACOES.md`. Não publique nem crie releases externos sem autorização explícita.
