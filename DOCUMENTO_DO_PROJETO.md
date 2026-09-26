# Documento do projeto

## Objetivo

Criar um aplicativo instalado no computador para fazer chamadas de voz e compartilhar a tela com amigos. O aplicativo será desenvolvido primeiro para Windows e distribuído como instalador.

## Primeira versão

O primeiro protótipo deve permitir:

- informar um nome de exibição;
- criar ou entrar em uma sala privada;
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

Ao criar uma sala, o aplicativo inicia um servidor de **sinalização** no PC do anfitrião, na porta 9000. O servidor ajuda os participantes a se encontrarem e troca as informações necessárias para iniciar a conexão. Ele não encaminha áudio nem tela quando o P2P direto funciona. O anfitrião compartilha o endereço `ws://IP:9000` e o código da sala; o convidado informa ambos no aplicativo.

O aplicativo lista os adaptadores ativos e seus IPv4 para escolher entre a rede local e uma rede virtual, como Radmin VPN. O Radmin e a entrada dos participantes na mesma rede virtual são configurados fora do aplicativo. O anfitrião pode precisar liberar a porta 9000 no firewall do Windows.

Se o anfitrião quiser sair com o outro participante conectado, o aplicativo solicita que ele aceite assumir a hospedagem. O novo anfitrião inicia o servidor no próprio PC com o mesmo código; o servidor anterior só encerra depois da confirmação. Se a transferência for recusada ou falhar, o anfitrião original permanece na sala e pode tentar novamente ou encerrá-la. Uma queda abrupta do PC anfitrião encerra a sala. Sem nenhum participante online, a sala não fica acessível automaticamente.

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
5. Integrar o servidor de sinalização Rust ao aplicativo e testar a transferência de anfitrião.
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

- A sala será entre duas pessoas ou poderá incluir vários amigos?
- Será implementado TURN como alternativa para redes que bloqueiam P2P direto?
- Como será feito o acesso externo ao servidor integrado caso o roteador ou o provedor bloqueie conexões de entrada?

## Referências técnicas

- [WebRTC: conexão entre participantes e sinalização](https://webrtc.org/getting-started/peer-connections)
- [WebRTC: servidor TURN](https://webrtc.org/getting-started/turn-server)
