# Como usar o P2P — Voz e tela

O aplicativo compartilha a tela e pode incluir, opcionalmente, o áudio reproduzido pelo Windows. Isso não é uma chamada de voz: o microfone, a câmera e o chat de texto não são transmitidos. A captura local fica na memória; vídeo e áudio não são gravados.

## Instalar o aplicativo

1. Baixe `P2P-Voz-e-tela-Setup.exe` no GitHub Release mais recente e execute-o. A instalação é feita no seu usuário e não exige administrador.
2. Abra **P2P - Voz e tela** pelo menu Iniciar.
3. O app lembra endereço do anfitrião, STUN, ganho de áudio, inclusão do som do computador, modo de criação, opção TURN, autorização para hospedagem e adaptador de controle. As preferências ficam neste computador e sobrevivem às atualizações. O código da sala e credenciais temporárias não são salvos.
4. Se preferir, o release também oferece `p2p-client.exe` para executar sem o instalador.

Ao desinstalar pelo Windows, as preferências e a foto do perfil são removidas. Os logs de diagnóstico permanecem em `%LOCALAPPDATA%\P2P-Voz-e-tela\logs`.

## Perfil de participante

Na tela inicial, **Seu perfil** permite informar um nome e escolher uma foto; ambos são opcionais. O nome aparece para os participantes em salas locais/Radmin e no modo Internet. Se ficar vazio, o aplicativo mostra **Participante N**. A foto é reduzida a uma miniatura JPEG de até 96×96 e 16 KiB, salva neste computador e enviada somente em salas locais/Radmin. No modo Internet, a foto não é enviada. O nome e a foto ficam salvos entre aberturas e atualizações. O perfil passa a valer quando você entrar na próxima sala.

## Antes de começar

- Cada pessoa precisa executar o `p2p-client.exe` no Windows 10 versão 1803 ou posterior, ou Windows 11.
- Salas locais/Radmin aceitam até oito participantes. O compartilhamento em grupo com sessões correlacionadas exige a versão 1.1.3 em todos os participantes.
- Se o Windows Firewall perguntar, permita o aplicativo nas redes que você está usando. Não é preciso desligar o firewall.
- Em **Configurações > Atualizações**, é possível procurar atualizações. Downloads e reinicialização para aplicar são iniciados por você e não ficam disponíveis durante uma sala.

## Rede local ou Radmin VPN

1. As duas pessoas entram na mesma rede local ou na mesma rede Radmin VPN.
2. O anfitrião seleciona **Rede local / Radmin** e clica em **Criar sala**. Escolhe o adaptador cujo IPv4 o amigo pode alcançar e compartilha o endereço `ws://IP:9000` e o código da sala.
3. O convidado seleciona **Rede local / Radmin** para escolher seu adaptador de controle, abre **Configurações > Conexão**, informa o endereço do anfitrião e volta à tela inicial. Digita o código e clica em **Entrar**.
4. Os participantes escolhem uma tela ou janela. Em uma sala compatível com grupo, cada pessoa pode clicar em **Compartilhar minha tela**; quem quiser assistir escolhe **Assistir** na transmissão correspondente. As telas assistidas aparecem na grade. Em uma sala com cliente antigo, o compartilhamento continua no fluxo de duas pessoas, com pedido e aceite.

## Áudio reproduzido pelo computador

Antes de iniciar o compartilhamento, marque **Incluir som do computador** na barra da sala para enviar também o áudio que está tocando na saída padrão do Windows. A opção fica desligada por padrão e não usa o microfone. O áudio segue diretamente pela conexão WebRTC para quem recebe aquela tela; o servidor da sala não recebe a mídia. Para cada tela assistida, o app reproduz o áudio remoto pela saída padrão do participante.

Se a captura de áudio falhar, o vídeo continua. Se a reprodução remota falhar, a tela continua sendo exibida. O app registra erros identificados por sessão e faixa, além de resumos a cada cinco segundos de captura, codificação Opus, pacotes RTP, decodificação e fila de reprodução. Use **Exportar logs** para salvar esse diagnóstico. Em salas com vários participantes, cada tela tem sua própria faixa de áudio; ouvir várias ao mesmo tempo pode misturar os sons.

Portas para permitir no firewall:

- TCP 9000 no PC anfitrião, para a sinalização da sala.
- TCP 9001 em cada PC, para controle e sucessão do anfitrião.
- UDP 9002–9009 nos PCs, para as sessões de vídeo em grupo.

No Radmin, escolham o adaptador e o endereço IPv4 da VPN. Todos precisam estar conectados à mesma rede virtual. A sucessão só funciona se os participantes elegíveis permitirem hospedagem e a malha de controle conseguir se conectar.

O compartilhamento em grupo requer a versão 1.1.3 em todos os participantes, pois cada sessão usa um identificador próprio para ofertas, respostas e ICE. Versões anteriores podem continuar usando o fluxo tradicional entre duas pessoas, mas o compartilhamento em grupo fica desativado até todos atualizarem. A sala aceita até oito participantes, mas cada PC tem oito portas UDP de mídia disponíveis (9002–9009); sessões simultâneas de envio e recepção podem atingir esse limite antes de a sala chegar a oito pessoas. Se isso ocorrer, pare de assistir a uma tela que não seja necessária e tente novamente.

## Internet entre duas casas — teste controlado

O modo Internet aceita apenas duas pessoas. O anfitrião precisa permanecer online; se sair ou cair, a sala termina. É necessário um IPv4 público alcançável ou um nome DDNS apontando para ele. CGNAT pode impedir conexões de entrada.

1. O anfitrião configura seu IPv4 público ou DDNS em **Configurações > Conexão** e encaminha TCP 9000 no roteador para o próprio PC. Permite o aplicativo no firewall.
2. Na tela inicial, seleciona **Internet (teste)**. A opção **Usar TURN como alternativa** vem ativada. Com TURN ativo, também encaminha UDP 3478 e UDP 50000–50100 no roteador para o PC anfitrião e permite essas portas no firewall.
3. Clica em **Criar sala** e envia ao amigo o endereço público `ws://...:9000` e o código da sala.
4. O convidado configura esse endereço em **Configurações > Conexão**, digita o código na tela inicial e clica em **Entrar**. O servidor informa que a sala está no modo Internet.
5. Os dois escolhem uma tela ou janela. O vídeo usa conexão direta quando possível; se ICE não encontrar um caminho direto e o anfitrião ativou TURN, o vídeo pode ser retransmitido pelo PC anfitrião.

Os dois PCs devem permitir UDP 9002 no firewall do Windows para a tentativa de vídeo direto. No roteador do anfitrião, encaminhe TCP 9000 e, se TURN estiver ativo, UDP 3478 e UDP 50000–50100. Se a conexão direta não funcionar e TURN não estiver disponível, o app não retransmitirá a mídia por outro serviço.

STUN ajuda a procurar uma rota direta e não retransmite mídia. TURN retransmite vídeo, aumenta o uso de banda do anfitrião e depende das portas UDP acima. Se TURN for desativado, a conexão depende de um caminho direto. UDP 9002 é usado pelo vídeo direto nos dois PCs.

**Segurança:** a sinalização da versão 1.0.0 usa `ws://` sem criptografia ou autenticação. Credenciais TURN temporárias também passam por esse canal. Use o modo Internet apenas em testes controlados com pessoas conhecidas; não reutilize códigos ou credenciais e não considere essa conexão privada contra terceiros na rede.

## Diagnóstico

- Se não conectar à sala, confira o endereço, o código, TCP 9000, o encaminhamento do roteador e o firewall do anfitrião.
- Se a sala conectar, mas o vídeo não, confira a mensagem ICE, a URI STUN e as portas UDP. O anfitrião também deve conferir UDP 3478 e UDP 50000–50100 quando TURN estiver ativo.
- **Configurações > Atualizações** permite verificar, baixar e aplicar uma versão do GitHub Releases. Quem ainda usa o `.exe` 1.0.0 com o atualizador antigo precisa instalar manualmente a versão migrada uma vez.
- Use **Exportar logs** para salvar diagnóstico local. Os logs podem incluir endereços IP e nomes de adaptadores; revise o arquivo antes de compartilhar.

## Atualizações publicadas pelo GitHub

O atualizador consulta o último release público do [repositório no GitHub](https://github.com/BadTonho/P2P/releases/latest) e procura o asset `p2p-client.exe`. Consulte [ATUALIZACOES.md](ATUALIZACOES.md) para publicar uma nova versão. Não é necessário criar ou enviar um manifesto separado.
