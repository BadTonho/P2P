# Publicar versões pelo GitHub Releases

O app consulta o último [GitHub Release público](https://github.com/BadTonho/P2P/releases/latest) pela API pública do repositório. Não precisa de login, chave de API ou `update-manifest.json`.

## Preparar uma versão

1. Atualize `[workspace.package].version` em `Cargo.toml` para uma versão maior no formato `MAJOR.MINOR.PATCH`.
2. Confirme que o compilador do Inno Setup 6 está disponível e abra o **Developer PowerShell for Visual Studio 2022**.
3. Na raiz do projeto, execute `.\scripts\build-installer.ps1`. Se o compilador não estiver no caminho padrão, informe `-InnoCompiler "C:\caminho\ISCC.exe"`. O script compila o cliente Windows em release e gera `target\installer\P2P-Voz-e-tela-Setup.exe`.
4. Crie e publique no GitHub um release estável com uma tag correspondente, como `v1.0.1`.
5. Anexe ao release `target\release\p2p-client.exe` com o nome exato `p2p-client.exe` e `target\installer\P2P-Voz-e-tela-Setup.exe` com o nome exato `P2P-Voz-e-tela-Setup.exe`.

O app encontra atualizações pelo asset `p2p-client.exe`; o instalador é um asset adicional para instalações novas. Repita o processo para cada versão: mude a versão, compile os dois arquivos e publique um novo release. A API do GitHub fornece a tag, o tamanho, o SHA-256 e o link do executável. O atualizador considera releases estáveis publicados, não rascunhos ou pré-lançamentos.

## Instalação, preferências e remoção

O instalador é por usuário, não exige administrador e instala em `%LOCALAPPDATA%\Programs\P2P-Voz-e-tela`. Ele cria um atalho no menu Iniciar. O atualizador integrado substitui o executável nessa pasta; as preferências ficam separadas em `%LOCALAPPDATA%\P2P-Voz-e-tela\settings.json` e sobrevivem às atualizações.

Ao desinstalar, o instalador remove o arquivo de preferências. O aplicativo não grava códigos de sala nem credenciais temporárias nesse arquivo. Os logs ficam na pasta `logs` e não são apagados junto com as preferências.

## Validação e segurança

O app baixa o executável por HTTPS e confere o tamanho, o SHA-256 informado pela API do GitHub e a assinatura PE (`MZ`). O aplicador auxiliar mantém uma cópia de recuperação e tenta restaurar a versão anterior se a substituição ou inicialização falhar.

Não há assinatura digital. O SHA-256 detecta corrupção durante o download, mas não prova a identidade do publicador se o repositório ou a conta do GitHub forem comprometidos. Um repositório e seus releases públicos podem ser vistos e baixados por qualquer pessoa.

As atualizações são verificadas automaticamente ao iniciar e também podem ser verificadas em **Configurações > Atualizações**. O download e a aplicação exigem ação do usuário e ficam bloqueados durante uma sala. O atualizador não baixa nem instala em silêncio.
