# Documento do projeto

## Objetivo

Criar um aplicativo instalado no computador para fazer chamadas de voz e compartilhar a tela com amigos. O aplicativo será desenvolvido primeiro para Windows e distribuído como instalador.

## Primeira versão

O primeiro protótipo deve permitir:

- criar ou entrar em uma sala privada com até oito participantes;
- mostrar a ordem de entrada e a fila de sucessão do anfitrião;
- permitir que cada pessoa autorize ou não este computador a assumir a hospedagem;
- iniciar e encerrar uma chamada de voz;
- iniciar e parar o compartilhamento da tela;
- encerrar a chamada.

O aplicativo deve pedir permissão antes de transmitir a tela e mostrar claramente quando o compartilhamento estiver ativo.

Não haverá câmera nem chat de texto na primeira versão.

## Linguagem e interface

- **Rust** será a linguagem principal do aplicativo e do servidor integrado ao cliente.
- **egui e eframe** serão usados para criar a interface gráfica em Rust.
- **WebRTC** será usado para tentar transmitir voz e tela diretamente entre os computadores. Uma implementação Rust a avaliar é [webrtc-rs](https://github.com/webrtc-rs/webrtc).

## Como a conexão funcionará

O objetivo é usar conexão ponto a ponto (P2P): quando a conexão direta funcionar, o áudio e a tela irão do computador de quem compartilha diretamente para os computadores dos amigos.

Ao criar uma sala, o aplicativo inicia um servidor de **sinalização** no PC do anfitrião, na porta 9000. O servidor ajuda os participantes a se encontrarem e troca as informações necessárias para iniciar a conexão. Ele não encaminha áudio nem tela. O anfitrião compartilha o endereço `ws://IP:9000` e o código da sala; os convidados informam ambos no aplicativo. O limite atual da sala é oito participantes; voz e tela para grupos ficam para as etapas WebRTC posteriores.

O aplicativo lista os adaptadores ativos e seus IPv4 para escolher entre a rede local e uma rede virtual, como Radmin VPN. O Radmin e a entrada dos participantes na mesma rede virtual são configurados fora do aplicativo. O anfitrião pode precisar liberar a porta 9000 no firewall do Windows.

Todos os participantes mantêm uma malha direta WebSocket de controle pela porta TCP 9001, separada da sinalização em 9000. O aplicativo mostra o endereço local ou do Radmin VPN e informa que todos precisam permitir conexões de entrada nessa porta no firewall. Esse canal transporta apenas estado da sala, eleições, pulsos e métricas de saúde; não transporta áudio ou tela.

Ao sair normalmente, o anfitrião inicia uma eleição automática. Se ele ficar instável, a eleição começa quando a pior conexão dele tiver pelo menos cinco amostras e perda de 20% ou mais, ou após cinco pulsos consecutivos sem resposta. Se o anfitrião cair, os participantes iniciam a eleição após cinco segundos sem sinais. A fila compara a pior conexão de cada candidato pela perda de pulsos, depois jitter e latência; a ordem de entrada desempata. O novo anfitrião tenta abrir a porta 9000; se falhar ou não ficar pronto em dez segundos, o aplicativo tenta o próximo.

A sala mantém o código, a identidade e a ordem dos participantes que reconectarem. O anfitrião afastado por instabilidade continua participante, mas só volta a ser candidato após 30 segundos com perda abaixo de 5%. Se a rede se dividir, podem surgir salas duplicadas temporariamente; quando os canais diretos voltarem, os participantes escolhem um líder de modo determinístico e reconectam a ele. Também há uma ação para encerrar a sala sem sucessor. Se não houver candidato elegível online, a sala termina.

O WebRTC usa mecanismos de rede como ICE e STUN para tentar encontrar um caminho direto entre computadores que estão atrás de roteadores. A sinalização e o envio de mídia são partes diferentes da conexão.

### TURN como alternativa

Se uma rede impedir a conexão direta, um servidor TURN pode retransmitir o áudio e a tela entre os participantes. Nesse caso, os dados passam pelo TURN e a transmissão deixa de seguir o caminho direto entre os computadores.

Um servidor TURN exigiria um serviço sempre ligado durante a chamada e encaminharia o tráfego de voz e tela, exigindo mais capacidade de envio da conexão de internet. Ainda não foi decidido se o protótipo terá TURN como alternativa. Sem TURN, algumas redes podem não conseguir estabelecer a chamada.

## Notebook e acesso pela internet

O PC do anfitrião precisa permanecer ligado e conectado à rede enquanto a sala estiver ativa. Para amigos em outras casas, o servidor integrado precisa estar acessível pela rede externa; isso pode exigir configuração do roteador. Se a conexão do provedor estiver atrás de CGNAT, conexões externas podem exigir uma solução adicional.

A primeira prova será feita entre computadores na mesma rede Wi-Fi ou Radmin VPN. Depois, será testada a conexão direta entre casas diferentes.

## Etapas de desenvolvimento

1. Preparar Rust e criar o projeto para Windows.
2. Criar a janela e os controles com egui/eframe.
3. Testar uma chamada de voz entre dois computadores na mesma rede.
4. Adicionar a captura e a transmissão direta da tela.
5. Integrar sinalização, malha direta de controle, fila e eleição automática para até oito participantes.
6. Testar conexões P2P entre redes diferentes.
7. Avaliar TURN se a conexão direta falhar em algumas redes.
8. Gerar o instalador do aplicativo.

## Fora do escopo inicial

- chat de texto;
- câmera;
- feed ou funções de rede social;
- contas de usuário e histórico de mensagens;
- publicar ou hospedar um site.

## Pontos a decidir depois

- Será implementado TURN como alternativa para redes que bloqueiam P2P direto?
- Como será feito o acesso externo ao servidor integrado caso o roteador ou o provedor bloqueie conexões de entrada?

## Referências técnicas

- [WebRTC: conexão entre participantes e sinalização](https://webrtc.org/getting-started/peer-connections)
- [WebRTC: servidor TURN](https://webrtc.org/getting-started/turn-server)
