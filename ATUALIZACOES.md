# Publicar atualizações pelo GitHub Releases

O app consulta o último [GitHub Release público](https://github.com/BadTonho/P2P/releases/latest) pela API pública do repositório. Não precisa de login, chave de API ou `update-manifest.json`.

## Como publicar uma versão

1. Atualize `[workspace.package].version` em `Cargo.toml` para uma versão maior no formato `MAJOR.MINOR.PATCH` e compile o cliente Windows em `release`.
2. No GitHub, crie e publique um release estável com uma tag correspondente, como `v1.0.1`.
3. Anexe `target/release/p2p-client.exe` ao release com o nome exato `p2p-client.exe`. Não renomeie o asset.
4. Depois que o release estiver publicado, o app encontrará a nova versão ao verificar atualizações. O usuário escolhe quando baixar e reiniciar para aplicar.

Repita esse processo para cada versão: mude o número da versão, compile e publique outro release com o novo executável. O endereço consultado pelo app permanece fixo; a API informa a tag mais recente, o tamanho, o SHA-256 e o link de download do asset. O app considera releases estáveis publicados, não rascunhos ou pré-lançamentos.

## Migração do atualizador antigo

A compilação 1.0.0 preparada anteriormente consulta o Google Drive e não conhece o atualizador do GitHub. Se alguém já estiver usando essa versão, precisará instalar manualmente uma compilação com o atualizador GitHub antes que a pasta e a chave antigas sejam desativadas. Para essa migração, use uma versão maior que 1.0.0, como 1.0.1.

## Validação e segurança

O app baixa o asset por HTTPS e confere tamanho, SHA-256 informado pela API do GitHub e assinatura PE (`MZ`). O aplicador auxiliar mantém uma cópia de recuperação e tenta restaurar a versão anterior se a substituição ou inicialização falhar.

Não há assinatura digital. O SHA-256 detecta corrupção durante o download, mas não prova a identidade do publicador quando o próprio repositório ou a conta do GitHub são comprometidos. Um repositório e seus releases públicos podem ser vistos e baixados por qualquer pessoa.

As atualizações são verificadas automaticamente ao iniciar e também podem ser verificadas em **Configurações > Atualizações**. O download e a aplicação exigem ação do usuário e ficam bloqueados durante uma sala. O atualizador não baixa nem instala em silêncio.
