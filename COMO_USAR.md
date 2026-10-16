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
- Salas locais/Radmin aceitam até oito participantes. O compartilhamento em grupo com sessões correlacionadas exige que todos usem um build com suporte a IDs de sessão.
- Se o Windows Firewall perguntar, permita o aplicativo nas redes que você está usando. Não é preciso desligar o firewall.
- Em **Configurações > Atualizações**, é possível procurar atualizações. Downloads e reinicialização para aplicar são iniciados por você e não ficam disponíveis durante uma sala.

## Rede local ou Radmin VPN

1. As duas pessoas entram na mesma rede local ou na mesma rede Radmin VPN.
2. O anfitrião seleciona **Rede local / Radmin** e clica em **Criar sala**, mantendo **Anunciar sala nesta rede** ativo.
3. O convidado escolhe a sala em **Salas nesta rede** e clica em **Entrar**. Não precisa digitar IP nem código. A lista mostra o nome do anfitrião e a ocupação; encontrar uma sala nunca inicia a conexão sozinho.
4. Os participantes escolhem uma tela ou janela. Em uma sala compatível com grupo, cada pessoa pode clicar em **Compartilhar minha tela**; quem quiser assistir escolhe **Assistir** na transmissão correspondente. As telas assistidas aparecem na grade. Em uma sala com cliente antigo, o compartilhamento continua no fluxo de duas pessoas, com pedido e aceite.

## Descoberta e seleção de rede

A seleção de rede **Automática** prioriza o Radmin ativo e usa Ethernet/Wi-Fi quando ele não está disponível. Em **Configurações > Conexão**, **Manual** permite escolher outro adaptador. A lista de adaptadores se atualiza enquanto você está fora de uma sala; durante a sessão, a rede fica fixa até você sair.

Com o anúncio ativo, quem estiver na mesma rede poderá encontrar e entrar na sala sem informar o código. O anfitrião pode desativá-lo na criação ou em **Configurações > Conexão**. Para salas sem anúncio, clientes antigos ou quando a descoberta não funcionar, use **Entrada manual · endereço e código**.

A descoberta usa broadcast **UDP 8999** somente no adaptador selecionado. Permita essa porta nos PCs participantes, além de **TCP 9000–9001** e **UDP 9002–9009**. O aplicativo não altera regras do firewall. Salas desaparecem da lista após seis segundos sem anúncio. Os códigos recebidos pela descoberta ficam apenas em memória e não são salvos nem registrados nos logs. Redes virtuais que não transportem broadcast precisarão da entrada manual.

## Escolher a qualidade da tela

Antes de iniciar uma transmissão, escolha no modal a resolução e a cadência: **720p** ou **1080p**, e **30 FPS** ou **60 FPS**. Os quatro perfis são permitidos. A escolha é salva somente neste computador e restaurada na próxima vez; configurações antigas começam em **720p30**. A captura preserva a proporção da tela e não amplia fontes menores que o perfil escolhido.

As taxas abaixo são alvos configurados para o vídeo de saída. Em uma transmissão individual, vale o limite individual. Em grupo, o orçamento total é dividido entre os espectadores ativos, sem ultrapassar o limite individual de cada um.

| Perfil | Alvo individual / limite por espectador | Alvo total em grupo |
|---|---:|---:|
| 720p30 | 4 Mbps | 8 Mbps |
| 720p60 ou 1080p30 | 6 Mbps | 12 Mbps |
| 1080p60 | 8 Mbps | 16 Mbps |

Por exemplo, em 1080p60 com quatro espectadores, o orçamento total de 16 Mbps permite até cerca de 4 Mbps por espectador. Esses valores não são consumo fixo nem garantia de qualidade: o tráfego real depende do que aparece na tela e inclui dados adicionais de rede. O aplicativo não reduz o perfil automaticamente quando a conexão fica instável; escolha um perfil mais leve se isso acontecer. Em grupo, o custo de codificação também cresce com o número de espectadores.

O perfil define a cadência desejada, mas não garante que o computador consiga codificar todos os quadros. Em uma máquina sem capacidade para 60 FPS, a transmissão pode entregar menos quadros sem alterar a preferência salva. A aceleração de hardware e o fallback por CPU podem ter desempenhos diferentes.

O perfil escolhido ainda não é negociado com cada receptor. Versões antigas podem ter limites diferentes de decodificação ou aceleração; ao testar computadores com versões distintas, comece em 720p30 e atualize os clientes antes de avaliar os perfis maiores.

Na preferência de decodificação, **Automático** começa com DXVA e pode mudar para **CPU (OpenH264)** quando o decoder não acompanha os quadros recebidos, incluindo congestionamento persistente da fila. **Preferir DXVA** mantém a GPU mesmo com FPS baixo, usando CPU se DXVA estiver indisponível ou falhar. A escolha vale na próxima sessão de compartilhamento. Uma recuperação por fila cheia mantém o decoder ativo e aguarda um quadro completo de recuperação; a avaliação de desempenho continua entre essas recuperações.

## Áudio reproduzido pelo computador

Antes de iniciar o compartilhamento, marque **Incluir som do computador** na barra da sala para enviar também o áudio que está tocando na saída padrão do Windows. A opção fica desligada por padrão e não usa o microfone. O áudio segue diretamente pela conexão WebRTC para quem recebe aquela tela; o servidor da sala não recebe a mídia. Para cada tela assistida, o app reproduz o áudio remoto pela saída padrão do participante.

Se a captura de áudio falhar, o vídeo continua. Se a reprodução remota falhar, a tela continua sendo exibida. O app registra erros identificados por sessão e faixa, além de resumos a cada cinco segundos de captura, codificação Opus, pacotes RTP, decodificação e fila de reprodução. Use **Exportar logs** para salvar esse diagnóstico. Em salas com vários participantes, cada tela tem sua própria faixa de áudio; ouvir várias ao mesmo tempo pode misturar os sons.

O volume recebido em **0%** silencia a reprodução e continua salvo conforme sua escolha. Quando há áudio recebido e decodificado, o diagnóstico informa esse silêncio intencional. Os resumos de recepção incluem o volume efetivo de cada faixa.

Portas para permitir no firewall:

- TCP 9000 no PC anfitrião, para a sinalização da sala.
- TCP 9001 em cada PC, para controle e sucessão do anfitrião.
- UDP 8999 nos PCs, para encontrar salas automaticamente.
- UDP 9002–9009 nos PCs, para as sessões de vídeo em grupo.

No Radmin, a seleção automática usa o adaptador da VPN. Todos precisam estar conectados à mesma rede virtual. A sucessão só funciona se os participantes elegíveis permitirem hospedagem e a malha de controle conseguir se conectar.

O compartilhamento em grupo requer que todos usem um build com suporte a IDs próprios de sessão para ofertas, respostas e ICE. Builds anteriores podem continuar usando o fluxo tradicional entre duas pessoas, mas o compartilhamento em grupo fica desativado até todos atualizarem. A sala aceita até oito participantes, mas cada PC tem oito portas UDP de mídia disponíveis (9002–9009); sessões simultâneas de envio e recepção podem atingir esse limite antes de a sala chegar a oito pessoas. Se isso ocorrer, pare de assistir a uma tela que não seja necessária e tente novamente.

## Internet entre duas casas — teste controlado

O modo Internet aceita apenas duas pessoas. O anfitrião precisa permanecer online; se sair ou cair, a sala termina. É necessário um IPv4 público alcançável ou um nome DDNS apontando para ele. CGNAT pode impedir conexões de entrada.

1. O anfitrião configura seu IPv4 público ou DDNS em **Configurações > Conexão** e encaminha TCP 9000 no roteador para o próprio PC. Permite o aplicativo no firewall.
2. Na tela inicial, seleciona **Internet (teste)**. A opção **Usar TURN como alternativa** vem ativada. Com TURN ativo, também encaminha UDP 3478 e UDP 50000–50100 no roteador para o PC anfitrião e permite essas portas no firewall.
3. Clica em **Criar sala** e envia ao amigo o endereço público `ws://...:9000` e o código da sala. Esse endereço não cifra a sinalização. Para usar `wss://`, o anfitrião precisa disponibilizar um endpoint TLS externo que encaminhe as conexões ao servidor integrado.
4. O convidado configura esse endereço em **Configurações > Conexão**, digita o código na tela inicial e clica em **Entrar**. O servidor informa que a sala está no modo Internet.
5. Os dois escolhem uma tela ou janela. O vídeo usa conexão direta quando possível; se ICE não encontrar um caminho direto e o anfitrião ativou TURN, o vídeo pode ser retransmitido pelo PC anfitrião.

Os dois PCs devem permitir UDP 9002 no firewall do Windows para a tentativa de vídeo direto. No roteador do anfitrião, encaminhe TCP 9000 e, se TURN estiver ativo, UDP 3478 e UDP 50000–50100. Se a conexão direta não funcionar e TURN não estiver disponível, o app não retransmitirá a mídia por outro serviço.

STUN ajuda a procurar uma rota direta e não retransmite mídia. TURN retransmite vídeo, aumenta o uso de banda do anfitrião e depende das portas UDP acima. Se TURN for desativado, a conexão depende de um caminho direto. UDP 9002 é usado pelo vídeo direto nos dois PCs.

**Segurança:** o servidor integrado usa `ws://` sem criptografia de transporte. Códigos, metadados de sinalização e credenciais TURN temporárias passam por esse canal e podem ser observados na rede. O cliente aceita `wss://` se o endereço configurado apontar para um endpoint com TLS; o servidor integrado não configura TLS sozinho. Use o modo Internet apenas com pessoas de confiança e proteja o endpoint de sinalização antes de expô-lo a redes não confiáveis.

## Diagnóstico

- Se não conectar à sala, confira o endereço, o código, TCP 9000, o encaminhamento do roteador e o firewall do anfitrião.
- Se a sala conectar, mas o vídeo não, confira a mensagem ICE, a URI STUN e as portas UDP. O anfitrião também deve conferir UDP 3478 e UDP 50000–50100 quando TURN estiver ativo.
- **Configurações > Atualizações** permite verificar, baixar e aplicar uma versão do GitHub Releases. O atualizador prioriza `P2P-Voz-e-tela-Setup.exe` e usa `p2p-client.exe` como fallback quando o instalador não estiver no release.
- Use **Exportar logs** para salvar diagnóstico local. Os logs podem incluir endereços IP e nomes de adaptadores; revise o arquivo antes de compartilhar.
- **Diagnóstico** mostra o identificador **Build**, a rede escolhida e os contadores de consultas e anúncios de descoberta. Esses dados também aparecem na exportação, sem nomes nem códigos das salas descobertas. Para acompanhar os eventos durante um teste, ative **Detalhados** antes de reproduzir o problema.
- O contador de repetições do RTP original na saída acompanha reenvios separadamente das estatísticas RTX. Ele ajuda a investigar os avisos SRTP; um reenvio por NACK pode ser necessário para recuperar um pacote perdido.

## Atualizações publicadas pelo GitHub

O atualizador consulta o último release público do [repositório no GitHub](https://github.com/BadTonho/P2P/releases/latest), prioriza `P2P-Voz-e-tela-Setup.exe` e mantém `p2p-client.exe` como fallback. Consulte [ATUALIZACOES.md](ATUALIZACOES.md) para publicar uma nova versão. Não é necessário criar ou enviar um manifesto separado.

## Buscar novamente as salas da rede

Em **Rede local / Radmin**, o botão **Buscar novamente**, ao lado de **Salas nesta rede**, atualiza os adaptadores e solicita uma consulta imediata na rede selecionada. A interface mostra **Buscando…** por dois segundos e agrega cliques repetidos. A busca automática continua a cada dois segundos; uma consulta manual mantém as salas válidas e não prolonga a validade de anúncios antigos.

Se o serviço estiver ausente ou seu worker tiver encerrado, o botão tenta iniciar a descoberta novamente. Uma falha mostra a causa e mantém a entrada manual disponível. A ação fica desabilitada durante conexão, sala ativa ou atualização que bloqueie ações de sala, e não aparece no modo Internet. A seleção automática continua priorizando o Radmin ativo; a seleção manual segue a rede escolhida. Buscar não conecta a uma sala nem salva códigos recebidos.

## Nível de logs

Em **Configurações > Geral**, escolha **Desativados**, **Avisos e erros** ou **Detalhados**. O padrão é **Avisos e erros**; os logs detalhados ficam desligados. **Desativados** não cria arquivos de sessão e preserva os logs antigos. **Exportar logs** continua disponível por ação manual e, com os logs desativados, exporta somente o relatório de diagnóstico, sem histórico de eventos.

Para medir o vídeo, escolha **Detalhados** e marque **Registrar métricas periódicas de desempenho no log** nos dois computadores. Compartilhe uma tela por pelo menos 15 segundos e use **Exportar logs** em cada PC, de preferência durante a transmissão. O diagnóstico mostra o motor de codificação, decodificação, cópia ou 3D que o Windows atribuir ao processo, com média e pico por motor. A coleta ocorre aproximadamente a cada segundo e os resumos são registrados a cada cinco segundos. Quando o Windows ou o driver não fornecerem um contador válido, aparecerá `unavailable`; isso não significa uso de 0%.

As medições do vídeo também distinguem entrada direta da GPU, entrada pela CPU em um codificador de hardware e OpenH264. Registram chamadas de captura, conversão de cores, codificação, decodificação, espera na fila e preparação da imagem, além do tempo e dos bytes copiados da GPU para a CPU. Os tempos são durações das chamadas do aplicativo, incluindo esperas, e algumas etapas se sobrepõem; não representam tempo exclusivo de execução da GPU ou percentual de CPU. Os percentuais dos motores pertencem ao processo inteiro, incluindo interface e outras telas compartilhadas, e não devem ser somados como um percentual total da placa.

### Permissão de administrador do Windows

Quando o Windows mostra uma confirmação de administrador em uma área protegida, a imagem compartilhada pode ficar temporariamente parada no último quadro. Na captura de monitor, o aplicativo aguarda o acesso voltar e tenta retomar automaticamente depois de aceitar ou cancelar a mensagem, mantendo o compartilhamento ativo. O botão de parar continua cancelando essa espera.

A recuperação foi testada com falhas de acesso simuladas. A confirmação de administrador real e a retomada nos dois computadores ainda precisam de validação manual com o executável de desenvolvimento atualizado.

### Brilho ao clicar ou segurar o mouse

A captura de desktop e a apresentação do vídeo usam opacidade total; o canal alpha do DXGI/WGC e o estilo de interação da interface não devem diminuir o brilho dos pixels. O conversor GPU declara entrada RGB completa (0–255) e saída NV12 BT.601 limitada (16–235), conforme o conversor CPU e o shader. Logs detalhados registram o contrato de cor ao criar o conversor e as mudanças entre caminhos GPU e CPU, incluindo composição do cursor, sem salvar imagens.

Para conferir o caso real, use o executável de desenvolvimento atualizado em ambos os computadores. Compartilhe uma imagem fixa com preto, branco, cinzas e cores; compare áreas fora do cursor antes, durante e depois de segurar o botão esquerdo no transmissor e na imagem recebida. Repita com a prévia local, a grade, a imagem ampliada e a tela cheia, em 720p/60, 1080p/30 e 1080p/60. Compare CPU e DXVA no receptor. Mudanças intencionais do conteúdo compartilhado ou escurecimento causado por um diálogo do próprio Windows fazem parte da captura e não representam o mesmo defeito.

A reprodução do sintoma original entre PC e notebook permanece pendente. Os testes sintéticos verificam que alpha e estados de interação não alteram pixels fixos; eles não substituem essa comparação física.

### Diagnóstico de faixa verde e reprodução em 1080p/60

O receptor DXVA usa a altura alocada pela GPU para localizar o plano de cores NV12 e copia apenas a imagem visível. Uma superfície de 1088 linhas pode conter um vídeo de 1080 linhas; o preenchimento não deve aparecer na imagem. Os logs detalhados registram `surface_width`, `surface_height`, `visible_width`, `visible_height`, `crop_x`, `crop_y`, `pitch`, `uv_offset` e o adaptador na inicialização e quando o layout mudar.

A apresentação remota tenta o backend `nv12_glow` na grade, na visualização ampliada e na tela cheia. Os planos Y e UV são enviados como texturas e convertidos por shader, evitando a conversão NV12→RGBA na CPU e a preparação de `ColorImage` nesse caminho. A cópia DXVA→CPU continua necessária. Uma falha gráfica é registrada e a mesma sessão passa a usar RGBA; ela não tenta recriar o shader a cada quadro. Reiniciar a transmissão permite uma nova tentativa. Essa alternativa de apresentação é independente da preferência do decoder: **CPU (OpenH264)** continua disponível, e **Automático** e **Preferir DXVA** mantêm suas regras.

Para validar em computadores físicos:

1. Ative logs **Detalhados** e as métricas periódicas nos dois computadores; anote qual executável foi aberto em cada um.
2. Transmita PC → notebook em 720p/60, 1080p/30 e 1080p/60 por dois minutos em cada perfil. Inclua texto, padrões de cor, movimento e uma imagem parada. O perfil é um limite; confira a resolução efetiva, que pode ser menor que 1080p se a fonte for menor.
3. Confira as bordas e as cores na grade, na visualização ampliada e na tela cheia. Alterne a qualidade e pare/reinicie o compartilhamento dez vezes; confirme ausência da faixa verde, orientação correta e imagem atualizada.
4. Repita notebook → PC com os perfis possíveis da fonte. Compare com **CPU (OpenH264)** no receptor para separar problemas de DXVA e apresentação.
5. Exporte os logs de ambos. No caminho acelerado, `ui_nv12_shader_attempts`, `ui_nv12_upload_call` e `ui_nv12_shader_draw_call` devem ter amostras; `decoder_cpu_color_convert` e `ui_rgba_image_conversion` não devem ganhar amostras para esses quadros. Os tempos medem chamadas e esperas, não tempo exclusivo da GPU. Meça FPS com conteúdo em movimento; uma tela estática pode ser repetida a 1 FPS.

Aceite a correção visual apenas após confirmar ausência da faixa no notebook. Registre separadamente a redução do custo de conversão, o FPS sustentado, perdas e recuperação por quadro-chave. Um teste local do shader ou de rede em loopback não estabelece desempenho ou compatibilidade na AMD Vega 8 do outro computador.

## Excluir um aplicativo do áudio

Abra **Configurações > Áudio**, atualize a lista e escolha o aplicativo em **Não compartilhar o áudio de**. A escolha fica somente nas preferências deste computador e identifica o executável, não o PID. A captura exclui a instância selecionada e os processos filhos dela.

Se o aplicativo estiver fechado quando o compartilhamento de áudio começar, o som completo do computador será enviado até ele abrir; a captura então troca para o modo seletivo e pode haver uma breve pausa. Em Windows anterior ao build 20348, o vídeo continua, mas o áudio não é enviado quando uma exclusão está configurada. O áudio é transmitido normalmente quando a opção de inclusão de som do computador está desligada.
