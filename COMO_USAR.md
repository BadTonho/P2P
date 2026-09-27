# Como usar o P2P — Voz e tela

Este guia cobre a versão Windows 1.0.0. O aplicativo compartilha tela; chamada de voz, câmera e chat de texto ainda não estão disponíveis. A captura local fica na memória. Nenhum vídeo é gravado.

## Antes de começar

- Cada pessoa precisa executar o `p2p-client.exe` no Windows 10 versão 1803 ou posterior, ou Windows 11.
- Para compartilhar a tela, a sala precisa ter exatamente duas pessoas. Salas locais/Radmin aceitam até oito participantes, mas o compartilhamento em grupo ainda não está disponível.
- Se o Windows Firewall perguntar, permita o aplicativo nas redes que você está usando. Não é preciso desligar o firewall.
- Em **Configurações > Atualizações**, é possível procurar atualizações. Downloads e reinicialização para aplicar são iniciados por você e não ficam disponíveis durante uma sala.

## Rede local ou Radmin VPN

1. As duas pessoas entram na mesma rede local ou na mesma rede Radmin VPN.
2. O anfitrião seleciona **Rede local / Radmin** e clica em **Criar sala**. Escolhe o adaptador cujo IPv4 o amigo pode alcançar e compartilha o endereço `ws://IP:9000` e o código da sala.
3. O convidado seleciona **Rede local / Radmin** para escolher seu adaptador de controle, abre **Configurações > Conexão**, informa o endereço do anfitrião e volta à tela inicial. Digita o código e clica em **Entrar**.
4. Os dois escolhem uma tela ou janela. Quem quiser transmitir clica em **Compartilhar tela com meu amigo**; o outro participante aceita. Para encerrar, clique em **Parar compartilhamento**.

Portas para permitir no firewall:

- TCP 9000 no PC anfitrião, para a sinalização da sala.
- TCP 9001 em cada PC, para controle e sucessão do anfitrião.
- UDP 9002 nos dois PCs, para o vídeo direto.

No Radmin, escolham o adaptador e o endereço IPv4 da VPN. Todos precisam estar conectados à mesma rede virtual. A sucessão só funciona se os participantes elegíveis permitirem hospedagem e a malha de controle conseguir se conectar.

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
