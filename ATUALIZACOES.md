# Publicar versões pelo GitHub Releases

O app consulta o último [GitHub Release público](https://github.com/BadTonho/P2P/releases/latest) pela API pública do repositório. Não precisa de login, chave de API ou `update-manifest.json`.

A próxima versão preparada é **1.2.10**, para distribuir a compilação atual e comparar a transmissão entre redes diferentes. Publique os dois executáveis em um release estável com a tag **`v1.2.10`**, mantendo os nomes indicados abaixo. A versão anterior publicada/preparada foi 1.2.9; a nova numeração permite que esses clientes reconheçam a atualização.

Os caminhos, tamanhos, hashes e resultados desta preparação estão em [PREPARACAO_RELEASE_1.2.10.md](analises/PREPARACAO_RELEASE_1.2.10.md). O relatório anterior, 1.2.9, permanece em [PREPARACAO_RELEASE_1.2.9.md](analises/PREPARACAO_RELEASE_1.2.9.md).

## Preparar uma versão

1. Atualize `[workspace.package].version` em `Cargo.toml` para uma versão maior no formato `MAJOR.MINOR.PATCH`.
2. Confirme que o compilador do Inno Setup 6 está disponível e abra o **Developer PowerShell for Visual Studio 2022**.
3. Na raiz do projeto, execute `.\scripts\build-installer.ps1`. Se o compilador não estiver no caminho padrão, informe `-InnoCompiler "C:\caminho\ISCC.exe"`. O script compila o cliente Windows em release e gera `target\installer\P2P-Voz-e-tela-Setup.exe`.
4. Crie e publique no GitHub um release estável com uma tag correspondente, como `v1.0.1`.
5. Anexe ao release `target\release\p2p-client.exe` com o nome exato `p2p-client.exe` e `target\installer\P2P-Voz-e-tela-Setup.exe` com o nome exato `P2P-Voz-e-tela-Setup.exe`.

No PowerShell 7, antes de executar o script, use `$PSNativeCommandArgumentPassing = "Legacy"` nesta sessão. O script fornece definições ao Inno Setup com aspas embutidas; o modo Legacy preserva os caminhos com espaços. A preparação 1.2.7 foi validada com esse ajuste e Inno Setup 6.7.3.

O app encontra atualizações prioritariamente pelo instalador `P2P-Voz-e-tela-Setup.exe` (executando a instalação silenciosa para atualizar arquivos, registro e atalhos), com fallback automático para `p2p-client.exe` caso o release contenha apenas o executável. Repita o processo para cada versão: mude a versão, compile os dois arquivos e publique um novo release. A API do GitHub fornece a tag, o tamanho, o SHA-256 e o link do asset. O atualizador considera releases estáveis publicados, não rascunhos ou pré-lançamentos.

## Instalação, preferências e remoção

O instalador é por usuário, não exige administrador e instala em `%LOCALAPPDATA%\Programs\P2P-Voz-e-tela`. Ele cria um atalho no menu Iniciar. O atualizador integrado executa o instalador em modo silencioso (`/SILENT /CLOSEAPPLICATIONS`); as preferências ficam separadas em `%LOCALAPPDATA%\P2P-Voz-e-tela\settings.json` e sobrevivem às atualizações.

Ao desinstalar, o instalador remove o arquivo de preferências e a foto opcional do perfil (`profile-avatar.jpg`). O aplicativo não grava códigos de sala nem credenciais temporárias nesse arquivo. Os logs ficam na pasta `logs` e não são apagados junto com as preferências.

## Validação e segurança

O app baixa o instalador ou executável por HTTPS e confere o tamanho e o SHA-256 informados pela API do GitHub. Também valida a estrutura PE do arquivo (`MZ`, deslocamento do cabeçalho e marcador `PE\0\0`); isso não é uma assinatura digital Authenticode. Para o executável portátil direto, também confere o marcador de versão embutido no build. Essas verificações detectam arquivos incompatíveis ou corrompidos, mas não autenticam quem publicou o arquivo.

Não há assinatura digital. O SHA-256 detecta corrupção durante o download, mas não prova a identidade do publicador se o repositório ou a conta do GitHub forem comprometidos. Um repositório e seus releases públicos podem ser vistos e baixados por qualquer pessoa.

As atualizações são verificadas automaticamente ao iniciar e também podem ser verificadas em **Configurações > Atualizações**. O download e a aplicação exigem ação do usuário e ficam bloqueados durante uma sala. O atualizador não baixa nem instala em silêncio.
