linux.about.ageday = { $count ->
    [one] { $formatted } giorno fa
    [many] { $formatted } di giorni fa
   *[other] { $formatted } giorni fa
    }

linux.about.agehour = { $count ->
    [one] { $formatted } ora fa
    [many] { $formatted } di ore fa
   *[other] { $formatted } ore fa
    }

linux.about.ageminute = { $count ->
    [one] { $formatted } minuto fa
    [many] { $formatted } di minuti fa
   *[other] { $formatted } minuti fa
    }

linux.about.app = app desktop per linux

linux.about.block = { $app }{ $break }{ $journal }

linux.about.journal = diario

linux.about.lastseen = { $journal } · visto l'ultima volta { $age }

linux.about.now = proprio ora

linux.about.unknown = diario sconosciuto

linux.about.version = { $name } { $version }

linux.about.versionarch = { $name } { $version } · { $arch }

linux.about.versionbuild = { $name } { $version } ({ $build })

linux.about.versionbuildarch = { $name } { $version } ({ $build }) · { $arch }

linux.about.versionbuildos = { $name } { $version } ({ $build }) · { $os }

linux.about.versionbuildosarch = { $name } { $version } ({ $build }) · { $os } · { $arch }

linux.about.versionbuildosversion = { $name } { $version } ({ $build }) · { $os } { $osversion }

linux.about.versionbuildosversionarch = { $name } { $version } ({ $build }) · { $os } { $osversion } · { $arch }

linux.about.versionos = { $name } { $version } · { $os }

linux.about.versionosarch = { $name } { $version } · { $os } · { $arch }

linux.about.versionosversion = { $name } { $version } · { $os } { $osversion }

linux.about.versionosversionarch = { $name } { $version } · { $os } { $osversion } · { $arch }

linux.bin.solstoneterminal.couldnotinstallsiginthandler = non è stato possibile installare il gestore di { $literal1 }: { $err }

linux.bin.solstoneterminal.couldnotinstallsigtermhandler = non è stato possibile installare il gestore di { $literal1 }: { $err }

linux.bin.solstoneterminal.executablemustbeabsolute = { $value1 }: l'eseguibile deve essere assoluto

linux.bin.solstoneterminal.nothinghere = { $value1 }: niente qui

linux.bin.solstoneterminal.stillrunning = { $value1 }: ancora in esecuzione

linux.browser.status.browserconnected = browser: { $value1 } connesso

linux.browser.status.browserpagesthiscomputerwaiting = { $ws1 }{ $value1 } pagine del browser su questo computer, in attesa di arrivare nel tuo diario

linux.browser.status.chromeedge = Chrome o Edge

linux.browser.status.ifyouhaventyet = { $ws1 }se non l'hai ancora fatto, aggiungi l'estensione di solstone a { $served }: { $url }

linux.browser.status.n0kb = { $value1 } KB

linux.browser.status.n1mb = { $value1 } MB

linux.cli.agreewithoutbeingasked = accetta senza che ti venga chiesto

linux.cli.anothercopyofthesolstoneappisalreadyrunning = Un'altra copia dell'app di solstone è già in esecuzione per questo login, quindi questa non si è avviata. Ferma l'altra copia, poi riprova.

linux.cli.browserendpointisnottrusted = l'endpoint del browser non è attendibile

linux.cli.cachex = cache:{ $ws1 }{ $value1 }

linux.cli.cachexnotcreatedyet = cache:{ $ws1 }{ $value1 } (non ancora creata)

linux.cli.commandoutputexceededitsbound = l'output del comando ha superato il suo limite

linux.cli.commandtimedout = tempo del comando scaduto

linux.cli.configupdateerrorx = errore di aggiornamento della config: { $error }

linux.cli.configx = config: { $value1 }

linux.cli.confirmyourjournalsmarkexitcodes0pairedand = conferma la marca del tuo diario.{ $ws1 }Codici di uscita:{ $ws2 }0{ $ws3 }accoppiato e confermato, o già confermato{ $ws4 }1{ $ws5 }non accoppiato, niente è cambiato, o nessun terminale{ $ws6 }2{ $ws7 }errore di utilizzo{ $ws8 }5{ $ws9 }trattenuto

linux.cli.couldnotdiscardthebrowserpagesx = non è stato possibile scartare le pagine del browser: { $error }

linux.cli.couldnotstartx = non è stato possibile avviare: { $error }

linux.cli.couldntkeepthatlinkforthe = non è stato possibile tenere quel link per la finestra: { $error }

linux.cli.credentialabsent = credenziale assente

linux.cli.discardfailed = non è stato possibile scartare

linux.cli.discardreplytoolarge = risposta di scarto troppo grande

linux.cli.discardwaitingbrowserpages = scarta le pagine del browser in attesa

linux.cli.drawcursor = Disegna il cursore

linux.cli.editsettings = modifica impostazioni

linux.cli.enterapositiveinteger = Inserisci un intero positivo.

linux.cli.enteryorn = Inserisci { $literal1 } o { $literal2 }.

linux.cli.erroreditingsettingsx = Errore nella modifica delle impostazioni: { $error }

linux.cli.errorx = Errore: { $error }

linux.cli.howmanyminutestopausefordefaultuntilyouresu = Per quanti minuti mettere in pausa (predefinito: finché non riprendi)

linux.cli.installgnomeandkwinactivityhelperassets = installa le risorse degli ausiliari dell'attività di { $command1 } e { $command2 }

linux.cli.installsystemduserservice = Installa il servizio utente di systemd

linux.cli.invaliddiscardreply = risposta di scarto non valida

linux.cli.journalversionx = versione del diario: { $version-str }

linux.cli.linkedprivatestateisalreadyinuse = Lo stato privato collegato è già in uso

linux.cli.missingexecutabledirectory = directory dell'eseguibile mancante

linux.cli.moveexistingsolstonetmuxinto = sposta un solstone esistente per { $command0 } in questa app

linux.cli.nosessionbussothereisnodesktoptosetupx = nessun bus di sessione, quindi non c'è un desktop da configurare: { $error }

linux.cli.notset = (non impostato)

linux.cli.pairthisdevicewithyourjournal = accoppia questo dispositivo con il tuo diario

linux.cli.pairthisdevicewithyourjournalexitcodes0pair = accoppia questo dispositivo con il tuo diario.{ $ws1 }Codici di uscita:{ $ws2 }0{ $ws3 }accoppiato e confermato, o già confermato{ $ws4 }1{ $ws5 }non accoppiato, niente è cambiato, o nessun terminale{ $ws6 }2{ $ws7 }errore di utilizzo{ $ws8 }5{ $ws9 }trattenuto

linux.cli.pauseintake = metti in pausa l'ingresso

linux.cli.resumeintake = riprendi l'ingresso

linux.cli.screenaudiodesktopactivityoff = schermo, audio e attività del desktop: attivo o disattivo

linux.cli.segmentdurationinsecondsdefault300 = Durata del segmento in secondi (predefinito: 300)

linux.cli.segmentintervalseconds = Secondi di intervallo del segmento

linux.cli.servicex = { $ws1 }servizio: { $value1 }

linux.cli.settingsboolprompt = { $label } [{ $answer }]:{ $ws1 }

linux.cli.settingsintprompt = { $label } [{ $current }]:{ $ws1 }

linux.cli.settingssavedtox = { $ws1 }Impostazioni salvate in { $value1 }

linux.cli.setupappalreadyrunning = non è stato possibile avviare la configurazione iniziale perché l'app di solstone è in esecuzione. ferma prima l'app di solstone e riprova. nessun input è stato consumato; lo stato dell'app, la config e lo stato privato sono invariati.

linux.cli.setupconfigfailed = configurazione iniziale non riuscita prima dell'accoppiamento perché l'app di solstone non ha potuto aggiornare in modo sicuro la sua config.

linux.cli.setupconnectionfailed = configurazione iniziale non riuscita: l'app di solstone non è riuscita a connettersi al tuo diario.

linux.cli.setupfailedx = configurazione iniziale non riuscita: { $error }

linux.cli.setupthegnomepaneliconwherepauseandresumel = configura l'icona del pannello di { $command1 }, dove si trovano pausa e ripresa

linux.cli.showstatus = mostra lo stato

linux.cli.showwhetherterminaliswhat = mostra se il terminale è attivo e cosa sta facendo

linux.cli.showyourappandjournalversions = mostra le versioni della tua app e del tuo diario

linux.cli.startpaused = Avvia in pausa

linux.cli.startupcouldnotcontinuebecausethesolstoneapp = Non è stato possibile continuare l'avvio perché l'app di solstone non ha potuto cancellare lo stato di sincronizzazione dell'esecuzione precedente. Assicurati che l'app di solstone possa scrivere i suoi dati locali, poi riprova.

linux.cli.startupcouldnotcontinuebecausethesolstoneapp2 = Non è stato possibile continuare l'avvio perché l'app di solstone non ha potuto preparare in modo sicuro i suoi dati locali. Assicurati che l'app di solstone possa scrivere i suoi dati locali, poi riprova.

linux.cli.streamnamedefaultstohostnamederived = Nome del flusso (predefinito: derivato dall'hostname)

linux.cli.streamx = flusso: { $stream }

linux.cli.terminaloffstatus = terminale: { $command3 } o stato

linux.cli.thesetakeeffectthenexttimethesolstoneappsta = Queste hanno effetto la prossima volta che l'app di solstone si avvia.

linux.cli.thesolstoneappforlinuxtakesinwhatyousharew = l'app di solstone per linux prende ciò che condividi con lei, e tutto va nel tuo diario. parte di solstone.

linux.cli.turnterminal = attiva il terminale

linux.cli.turnterminaloff = disattiva il terminale

linux.cli.uninstallsystemduserservice = Disinstalla il servizio utente di systemd

linux.cli.verifyinstallprerequisites = Verifica i prerequisiti di installazione

linux.cli.windowscopeexitedx = ambito della finestra uscito: { $status }

linux.cli.xincompletesegments = { $ws1 }{ $value1 } segmenti incompleti

linux.cli.xsegmentsacrossxdaysxmb = { $ws1 }{ $value1 } segmenti su { $value2 } giorni, { $value3 } MB

linux.clipboard.agentinstructions = l'app di solstone per linux (repository: { $repo })
    Fonte: { $repository }
    Leggi INSTALL.md su { $install } per la configurazione iniziale e l'architettura.
    Config: { $config_path }
    Segmenti: { $captures_dir }
    Registri: { $logs }
    Servizio: { $service }

linux.control.screenandaudioneedyourdesktopsession = schermo e audio: serve la tua sessione desktop, e nessuna è in esecuzione

linux.control.solstonefortmuxhadbeenstoppedso = { $ws1 }solstone per { $literal2 } era stato fermato, quindi il terminale resta disattivo

linux.control.terminalhistorycontinued = { $ws1 }cronologia del terminale: continuata

linux.control.terminalhistorycontinuedundertheoldpairing = { $ws1 }cronologia del terminale: continuata, sotto il vecchio accoppiamento

linux.control.thesolstoneappdidnotanswerin = l'app di solstone non ha risposto in tempo; riprova tra un momento.

linux.control.thesolstoneappisntrunning = { $ws1 }l'app di solstone non è in esecuzione

linux.control.thesolstoneappisntrunningstart = l'app di solstone non è in esecuzione; avviala e riprova.

linux.control.thesolstoneappisstartingtryagain = l'app di solstone si sta avviando; riprova tra un momento

linux.control.thisneedsattentionpleasecontactsupport = { $ws1 }questo richiede attenzione: per favore contatta il supporto

linux.copy.aboutheading = informazioni su solstone

linux.copy.aboutmadeby = fatto da { $literal1 }.

linux.copy.activityonly = solo attività

linux.copy.activityroute = percorso dell'attività

linux.copy.anythingwaitingsyncstaysthis = tutto ciò che è in attesa di sincronizzazione resta su questo computer. per vedere cosa fare, esegui: { $literal1 }

linux.copy.anythingwaitingtosyncstaysonthiscomputerthe = tutto ciò che è in attesa di sincronizzazione resta su questo computer. l'app di solstone riproverà quando ci si può connettere al tuo diario.

linux.copy.appsleftoutofdesktopactivity = app lasciate fuori dall'attività del desktop

linux.copy.appversion = versione dell'app

linux.copy.attentioncount = { $count ->
    [one] { $formatted } richiede attenzione
    [many] { $formatted } richiedono attenzione
   *[other] { $formatted } richiedono attenzione
    }

linux.copy.audio = audio

linux.copy.audiointakecancontinue = l'ingresso audio può continuare.

linux.copy.availablelogsareshownbelowcheckthenotesforg = i registri disponibili sono mostrati sotto. controlla le note per le lacune prima di salvare una copia.

linux.copy.cancelpairing = annulla l'accoppiamento

linux.copy.checkingstatus = controllo dello stato…

linux.copy.checkstatus = controlla lo stato

linux.copy.choose = scegli

linux.copy.choosefolder = scegli una cartella

linux.copy.choosefolder.v4cba918b = scegli una cartella…

linux.copy.chooseobsidianvault = scegli il tuo vault { $literal1 }

linux.copy.configfile = file di configurazione

linux.copy.confirming = conferma in corso…

linux.copy.confirmyourjournalsmark = conferma la marca del tuo diario

linux.copy.connected = connesso a

linux.copy.connecting = connessione in corso…

linux.copy.connectjournal = connettiti al tuo diario

linux.copy.copysaved = copia salvata.

linux.copy.couldntcheckwhetherallyoursettingsweresaved = non è stato possibile controllare se tutte le tue impostazioni sono state salvate. leggile di nuovo prima di riprovare.

linux.copy.couldntcheckwhetherintakepausedcheckstatusbe = non è stato possibile controllare se l'ingresso è stato messo in pausa. controlla lo stato prima di riprovare.

linux.copy.couldntcheckwhetherintakeresumedcheckstatusb = non è stato possibile controllare se l'ingresso è stato ripreso. controlla lo stato prima di riprovare.

linux.copy.couldntcheckwhetherpairingfinishedcheckstatus = non è stato possibile controllare se l'accoppiamento è terminato. controlla lo stato prima di riprovare.

linux.copy.couldntopenthatlinkvisitsupportsolstoneappo = non è stato possibile aprire quel link. visita { $literal1 } o scrivi a { $literal2 }.

linux.copy.couldntopenthesolstonewindow = non è stato possibile aprire la finestra di solstone

linux.copy.couldntopenthesolstonewindowopenxfordetails = non è stato possibile aprire la finestra di solstone. apri { $path } per i dettagli.

linux.copy.couldntopenyourbrowsertryagainoropenyourjo = non è stato possibile aprire il tuo browser. riprova, o apri il tuo diario dal suo indirizzo abituale.

linux.copy.couldntpauseintaketryagain = non è stato possibile mettere in pausa l'ingresso. riprova.

linux.copy.couldntreadthelogfilestryagain = non è stato possibile leggere i file di registro. riprova.

linux.copy.couldntreadthelogstryagain = non è stato possibile leggere i registri. riprova.

linux.copy.couldntreadthesolstoneappsstatuscheckwhethe = non è stato possibile leggere lo stato dell'app di solstone. controlla se è in esecuzione, poi riprova.

linux.copy.couldntreadyoursettingstryagainbeforemaking = non è stato possibile leggere le tue impostazioni. riprova prima di apportare modifiche.

linux.copy.couldntrestartthesolstoneappcheckthedetails = non è stato possibile riavviare l'app di solstone. controlla i dettagli, poi riprova.

linux.copy.couldntresumeintaketryagain = non è stato possibile riprendere l'ingresso. riprova.

linux.copy.couldntsaveactivityintaketryagain = non è stato possibile salvare l'ingresso dell'attività. riprova.

linux.copy.couldntsaveallyoursettingsyoureditsarestill = non è stato possibile salvare tutte le tue impostazioni. le tue modifiche sono ancora qui. riprova.

linux.copy.couldntsavethecopytrysavingtoanotherfolder = non è stato possibile salvare la copia. prova a salvarla in un'altra cartella.

linux.copy.couldntsaveyouranswertryagain = non è stato possibile salvare la tua risposta. riprova.

linux.copy.couldntsaveyoursettingscheckthedetailsthent = non è stato possibile salvare le tue impostazioni. controlla i dettagli, poi riprova.

linux.copy.couldntstartthesolstoneappcheckthedetailsth = non è stato possibile avviare l'app di solstone. controlla i dettagli, poi riprova.

linux.copy.couldntverify = non è stato possibile verificare

linux.copy.couldntverifyjournalso = non è stato possibile verificare il tuo diario, quindi non si è connesso niente. riprova, o incolla un link di accoppiamento.

linux.copy.couldntworkoutthismarksotheresnothingtoco = non è stato possibile determinare questa marca, quindi non c'è niente da confrontare. il tuo accoppiamento attuale resta in uso.

linux.copy.couldntworkoutyourjournalsmarksotheresnot = non è stato possibile determinare la marca di quel diario, quindi non c'è niente da confrontare. ottieni un nuovo link di accoppiamento dal tuo diario, poi riprova.

linux.copy.countwaitingsync = { $count } in attesa di sincronizzazione

linux.copy.d1 = mentre questo è attivo, l'app di solstone prende gli screenshot che fai su questo computer, e il tuo diario tiene ciascuno al minuto in cui l'hai fatto.

linux.copy.d10 = alcuni strumenti per gli screenshot salvano solo quando glielo chiedi, e uno screenshot che non è salvato non entra.

linux.copy.d15 = altro su questo

linux.copy.d8 = non è stato possibile trovare una cartella Screenshots su questo computer. scegli la cartella in cui si salvano i tuoi screenshot.

linux.copy.d8pickertitle = scegli la cartella in cui si salvano i tuoi screenshot

linux.copy.desktopactivity = attività sul desktop

linux.copy.desktopactivityisonwheneverthesolstoneapptak = l'attività sul desktop è attiva ogni volta che l'app di solstone prende il tuo schermo, e la pausa mette in pausa entrambi. l'app di solstone prende l'app, il titolo della finestra, gli indirizzi web e il tempo attivo o inattivo che condividi con lei, e tutto va nel tuo diario.

linux.copy.doesthismatchyourjournal = questo corrisponde al tuo diario?

linux.copy.drawcursor = disegna il cursore

linux.copy.enterawholenumbergreaterthan0 = inserisci un numero intero maggiore di 0.

linux.copy.foundjournalthiscomputer = trovato il tuo diario su questo computer

linux.copy.from = da { $folder }

linux.copy.getafreshlink = ottieni un nuovo link

linux.copy.gethelp = ottieni aiuto

linux.copy.healthdetails = dettagli dello stato

linux.copy.help = aiuto

linux.copy.helpreport = segnala un problema

linux.copy.intake = ingresso

linux.copy.intakeisntrunning = l'ingresso non è in esecuzione.

linux.copy.intakeispauseduntilyouresume = l'ingresso è in pausa finché non riprendi.

linux.copy.intakeisrunning = l'ingresso è in esecuzione.

linux.copy.journal = diario

linux.copy.journal3 = il tuo diario

linux.copy.journaldetails = dettagli del diario

linux.copy.journallinkinvalidbody = copia il link completo dalla tua chat e aprilo di nuovo.

linux.copy.journallinkinvalidtitle = questo link del diario non è valido.

linux.copy.journalneedsupdatebeforeit = il tuo diario ha bisogno di un aggiornamento prima di poter prendere queste note. sono in attesa su questo computer.

linux.copy.journalsetupaction = configura un diario su questo computer

linux.copy.journalsetupfailed = non è stato possibile preparare il tuo diario. esegui { $literal1 } in un terminale per vedere cosa richiede attenzione.

linux.copy.journalsetupinbrowser = termina la configurazione iniziale nel tuo browser.

linux.copy.journalsetupinstall = installa il diario

linux.copy.journalsetupnotinstalled = il diario non è ancora installato su questo computer.

linux.copy.journalsetupopenfailed = il tuo diario è stato preparato, ma il browser non si è aperto. esegui { $literal1 } in un terminale per terminare la configurazione iniziale.

linux.copy.journalsetuppreparing = preparazione del tuo diario…

linux.copy.keepdesktopactivityonwhilepaused = tieni l'attività sul desktop attiva durante la pausa

linux.copy.lastchecked = ultimo controllo

linux.copy.lastdelivery = ultima consegna

linux.copy.lastdeliveryvalue = ultima consegna: { $value }

linux.copy.linuxdoesnotclassifyprivatewindows = linux non classifica le finestre private.

linux.copy.mark = marca

linux.copy.markunavailable = marca non disponibile

linux.copy.moveit = spostalo

linux.copy.movesolstonetmux = spostare solstone dentro per { $literal1 }?

linux.copy.needahandreachahumanwerehappytohelp = ti serve una mano? contattaci. siamo felici di aiutarti.

linux.copy.needattention = richiedono attenzione

linux.copy.needsattention = richiede attenzione

linux.copy.newpairingcancelled = nuovo accoppiamento annullato

linux.copy.newpairingcancelledyourcurrentpairingstaysin = nuovo accoppiamento annullato. il tuo accoppiamento attuale resta in uso.

linux.copy.nologfilesareavailableyettryagainafterusing = non sono ancora disponibili file di registro. riprova dopo aver usato l'app.

linux.copy.norecentlogsareavailableyettryagainafterusi = non sono ancora disponibili registri recenti. riprova dopo aver usato l'app.

linux.copy.not = non

linux.copy.nothingthiscomputerhaskept = niente di ciò che questo computer ha tenuto è andato a quel diario tramite questo link. potresti aver incollato il link sbagliato, o qualcosa non va. ottieni un nuovo link di accoppiamento dal tuo diario e riprova, o scrivi a { $literal1 } e ti aiuteremo.

linux.copy.nothingwaitinggoesintoyourjournaluntilyoudo = niente in attesa va nel tuo diario finché non lo fai.

linux.copy.notpaired = non accoppiato

linux.copy.notreachingyourjournalrightnow = non ci si può connettere al tuo diario in questo momento

linux.copy.notthisdevice = non su questo dispositivo

linux.copy.notyet = non ancora

linux.copy.obsidian = { $literal1 }

linux.copy.obsidianwarning = il tuo diario ha già un vault chiamato { $vault } da un altro dispositivo. se continui, le modifiche da entrambi i dispositivi vanno nel tuo diario.

linux.copy.obsidianwarningdate = il tuo diario ha già un vault chiamato { $vault } da un altro dispositivo, aggiornato l'ultima volta { $date }. se continui, le modifiche da entrambi i dispositivi vanno nel tuo diario.

linux.copy.obsidianwarningdevice = il tuo diario ha già un vault chiamato { $vault } da { $device }. se continui, le modifiche da entrambi i dispositivi vanno nel tuo diario.

linux.copy.obsidianwarningdevicedate = il tuo diario ha già un vault chiamato { $vault } da { $device }, aggiornato l'ultima volta { $date }. se continui, le modifiche da entrambi i dispositivi vanno nel tuo diario.

linux.copy.off = disattivo

linux.copy.offmeansnonethreegoes = disattivo significa che nessuno dei tre va nel tuo diario, qualunque cosa stia facendo il tuo desktop.

linux.copy.ofinyourjournal = { $done } di { $total } nel tuo diario

linux.copy.ofscreenshotsinyourjournal = { $done } di { $total } screenshot nel tuo diario

linux.copy.omittedbytes = { $count ->
    [one] { $break }[omesso { $formatted } byte]{ $break }
    [many] { $break }[omessi { $formatted } di byte]{ $break }
   *[other] { $break }[omessi { $formatted } byte]{ $break }
    }

linux.copy.on = attivo

linux.copy.openthenetworkappinyourjournalandgetapairi = apri l'app della rete nel tuo diario e ottieni un link di accoppiamento.

linux.copy.openyourjournal = apri il tuo diario

linux.copy.otherfilesinstaywheretheyare = gli altri file in { $folder } restano dove sono.

linux.copy.pairadevice = accoppia un dispositivo

linux.copy.pairagain = accoppia di nuovo

linux.copy.pairing = accoppiamento in corso…

linux.copy.pairingcancelledgetafreshpairinglinkfromyour = accoppiamento annullato. ottieni un nuovo link di accoppiamento dal tuo diario quando vuoi.

linux.copy.pairingdidntgothroughgetanewpairinglinkfro = l'accoppiamento non è riuscito. ottieni un nuovo link di accoppiamento dal tuo diario e riprova.

linux.copy.pairinglink = link di accoppiamento

linux.copy.partthisvaultcant = una parte di questo vault non può essere letta. controlla i permessi sulle cartelle al suo interno. il resto del vault continua ad andare nel tuo diario.

linux.copy.pasteapairinglinkfromyourjournal = incolla un link di accoppiamento dal tuo diario.

linux.copy.pastepairinglink = incolla il link di accoppiamento

linux.copy.pause = pausa

linux.copy.paused = in pausa

linux.copy.pausing = pausa in corso…

linux.copy.presented = presentata

linux.copy.quit = esci da solstone

linux.copy.quitfailed = non è stato possibile fermare l'app di solstone. riprova.

linux.copy.readinglogs = lettura dei registri…

linux.copy.readrecentlogs = leggi i registri recenti

linux.copy.readsettingsagain = leggi di nuovo le impostazioni

linux.copy.readthelogsbelowthenchoosewhethertosaveaco = leggi i registri qui sotto, poi scegli se salvare una copia.

linux.copy.readthesolstoneappsrecentlogsbeforesavinga = leggi i registri recenti dell'app di solstone prima di salvare una copia per un rapporto di supporto. niente viene inviato da qui.

linux.copy.recentlogs = registri recenti

linux.copy.restarting = riavvio in corso…

linux.copy.restartneeded = riavvio necessario

linux.copy.restartthesolstoneapp = riavvia l'app di solstone

linux.copy.resume = riprendi

linux.copy.resuming = ripresa in corso…

linux.copy.s1 = screenshot

linux.copy.s10counting = controllo…

linux.copy.s10today = inizia da oggi

linux.copy.s11 = scegli quanto indietro iniziare, poi portali dentro.

linux.copy.s12 = non è stato possibile contare i tuoi screenshot.

linux.copy.s14 = uno screenshot già nel tuo diario resta lì se lo elimini dalla cartella.

linux.copy.s15 = ciò che è già nel tuo diario resta quando metti in pausa o disattivi questo.

linux.copy.s16computer = gli screenshot che fai mentre è in pausa non entrano.

linux.copy.s17 = gli screenshot già nel tuo diario non vengono presi di nuovo.

linux.copy.s19 = in attesa di andare nel tuo diario

linux.copy.s21 = in attesa di un aggiornamento del diario. i tuoi screenshot aspettano su questo computer.

linux.copy.s22 = cartella

linux.copy.s23 = arriva solo ciò che l'app di solstone può riconoscere come uno screenshot.

linux.copy.s7 = da prima di oggi

linux.copy.s9 = vedrai quanti screenshot ci sono prima che ne arrivi qualcuno.

linux.copy.saveacopy = salva una copia…

linux.copy.savesettings = salva le impostazioni

linux.copy.saving = salvataggio…

linux.copy.savingcopy = salvataggio della copia…

linux.copy.savingwritesexactlythepreviewabove = il salvataggio scrive esattamente l'anteprima sopra.

linux.copy.screen = schermo

linux.copy.screenandaudioarepauseduntilyouresumedesktop = lo schermo e l'audio sono in pausa finché non riprendi. l'attività sul desktop continua.

linux.copy.screenandaudiostillpausedesktopactivitykeeps = lo schermo e l'audio si mettono ancora in pausa. l'attività sul desktop continua.

linux.copy.screenaudiodesktopactivity = lo schermo, l'audio e l'attività sul desktop

linux.copy.screenintakeiswaiting = l'ingresso dello schermo è in attesa

linux.copy.screenshotslookback76 = gli ultimi 30 giorni

linux.copy.screenshotslookback77 = gli ultimi 6 mesi

linux.copy.screenshotslookback78 = l'ultimo anno

linux.copy.screenshotslookback79 = tutto quanto

linux.copy.screenshotslookback80 = niente da prima di oggi

linux.copy.seconds = secondi

linux.copy.segmentinterval = intervallo del segmento

linux.copy.sentfoundnotesjournal = { $sent } di { $found } note nel tuo diario

linux.copy.settings = impostazioni

linux.copy.settingsactivityonlyhelp = solo l'attività sul desktop, con lo schermo e il suono disattivi.

linux.copy.settingssaved = impostazioni salvate.

linux.copy.solstoneappcantread = l'app di solstone non può leggere questo vault. controlla che la sua cartella ci sia e che il disco su cui si trova sia connesso. niente cambia nel tuo diario mentre non è raggiungibile.

linux.copy.solstoneappcouldntsave = non è stato possibile salvare la posizione dell'app di solstone per questo vault. controlla che questo computer abbia spazio libero.

linux.copy.solstoneappdidntfind = l'app di solstone non ha trovato un vault su questo computer. scegli invece la sua cartella.

linux.copy.solstoneapptakesnotesone = l'app di solstone prende le note in un solo vault su questo computer. tutte vanno nel tuo diario, poi ciascuna quando cambia. il tuo vault resta com'è.

linux.copy.somenotescouldntgo = non è stato possibile far entrare alcune note nel tuo diario. l'app di solstone le riprova la prossima volta che si avvia.

linux.copy.somethingansweredinsteadjournal = qualcosa ha risposto al posto del tuo diario

linux.copy.somethingansweredthiscomputerat = qualcosa ha risposto a questo computer a { $address }, e non è il diario con cui questo computer è accoppiato. potrebbe essere un altro dispositivo che ora usa quell'indirizzo, o il tuo diario, se la sua identità è stata reimpostata. se la marca sotto "cosa ha risposto" corrisponde a quella che il tuo diario mostra ora, accoppia di nuovo questo computer.

linux.copy.somethingansweredthiscomputerthrough = qualcosa ha risposto a questo computer tramite il relay, e non è il diario con cui questo computer è accoppiato. questo è meno certo di una connessione diretta.

linux.copy.starting = avvio in corso…

linux.copy.startpaused = avvia in pausa

linux.copy.startthesolstoneapp = avvia l'app di solstone

linux.copy.statusunavailable = stato non disponibile

linux.copy.syncingjournal = sincronizzazione verso il tuo diario…

linux.copy.takestmuxsessionsallit = prende le tue sessioni { $literal1 }, e tutto va nel tuo diario. richiede { $literal1 } su questo computer, e funziona tramite { $literal2 } e senza desktop.

linux.copy.terminal = terminale

linux.copy.thatdoesntmatch = quello non corrisponde

linux.copy.thatfolderisntobsidian = quella cartella non è un vault { $literal1 }, quindi l'app di solstone non l'ha aggiunta. scegli la cartella propria del vault.

linux.copy.thatfoldersnamecan = il nome di quella cartella non può essere il nome di un vault nel tuo diario. rinomina la cartella e riprova.

linux.copy.thatpairinglinkisntonethesolstoneappcanrea = quel link di accoppiamento non è uno che l'app di solstone può leggere. ottieni un nuovo link di accoppiamento dal tuo diario e riprova.

linux.copy.thefolderisntonthiscomputer = la cartella { $folder } non è più su questo computer.

linux.copy.thelogpreviewwaslimitedmorelogtextmaybeava = { $ws1 }[l'anteprima del registro è stata limitata; altro testo del registro potrebbe essere disponibile]{ $ws1 }

linux.copy.thepairingchangedwhileyouwerecheckingitsmark = l'accoppiamento è cambiato mentre controllavi la sua marca. controlla la marca attuale prima di confermare.

linux.copy.thepairingwindowclosedgetanewpairinglinkfro = la finestra di accoppiamento si è chiusa. ottieni un nuovo link di accoppiamento dal tuo diario, poi riprova.

linux.copy.thesetakeeffectthenexttimethesolstoneappsta = hanno effetto la prossima volta che l'app di solstone si avvia.

linux.copy.thesolstoneappcannowconnecttoyourjournal = l'app di solstone ora può connettersi al tuo diario.

linux.copy.thesolstoneapphasntansweredyetcheckforades = l'app di solstone non ha ancora risposto. controlla se c'è una richiesta di condivisione del desktop, poi controlla lo stato.

linux.copy.thesolstoneapprestartedwithyoursavedsettings = l'app di solstone si è riavviata con le tue impostazioni salvate.

linux.copy.thewindowcouldntstartopenxfordetails = non è stato possibile avviare la finestra. apri { $path } per i dettagli.

linux.copy.thewindowcouldntstarttryopeningitagain = non è stato possibile avviare la finestra. prova ad aprirla di nuovo.

linux.copy.thewindowneedsgtk46ornewercouldntloadits = la finestra richiede { $literal1 } 4.6 o più recente. non è stato possibile caricare le sue librerie del desktop. apri { $path } per i dettagli.

linux.copy.thisisjournal = questo è il tuo diario

linux.copy.thismovessolstonetmuxinto = questo sposta solstone per { $literal1 } in questa app, e si ferma per sempre. ciò che ha ancora in attesa arriva nel tuo diario attraverso questa app, e ciò che non può arrivare resta su questo computer. se questa app non è ancora accoppiata, si accoppia con il diario che usa solstone per { $literal1 }.

linux.copy.thisstartsthesolstoneappnowandatsigninopen = questo avvia l'app di solstone ora e all'accesso. solo aprire questa finestra non avvia l'ingresso.

linux.copy.toolargebring = troppo grande da prendere

linux.copy.tryagain = riprova

linux.copy.turningthisoffstopsbringing = disattivare questo smette di prendere le modifiche. ciò che è già nel tuo diario resta lì, e ciò che è in attesa resta su questo computer finché non lo attivi di nuovo.

linux.copy.turnoff = disattiva

linux.copy.unavailable = non disponibile

linux.copy.updateshow = con gli aggiornamenti automatici attivi, l'app si aggiorna insieme al tuo diario quando il programma di installazione di solstone ha configurato entrambi su questo computer. altrimenti, esegui di nuovo il programma di installazione di solstone con --upgrade.

linux.copy.updatesjournalversion = versione del diario

linux.copy.usethisvault = usa questo vault

linux.copy.vault = vault

linux.copy.vaultsthiscomputer = i vault su questo computer

linux.copy.vaultwiththisnameis = un vault con questo nome è già nel tuo diario

linux.copy.versionvalue = versione dell'app: { $value }

linux.copy.viewdetails = vedi i dettagli

linux.copy.waitingforthesolstoneapptoanswerifyourdeskt = in attesa che l'app di solstone risponda. se il tuo desktop chiede che cosa condividere, fai la tua scelta lì.

linux.copy.waitingforyoutoconfirmyourjournalsmark = in attesa che tu confermi la marca del tuo diario

linux.copy.waitingjournalupdate = in attesa di un aggiornamento del diario

linux.copy.waitingreachjournal = in attesa di connettersi al tuo diario

linux.copy.waitingtosync = in attesa di sincronizzazione

linux.copy.waitingvalue = in attesa di sincronizzazione: { $value }

linux.copy.webaddressesturnsondesktopaccessibility = indirizzi web (attiva l'accessibilità del desktop)

linux.copy.whatanswered = cosa ha risposto

linux.copy.windowtitles = titoli delle finestre

linux.copy.windowtitlesleftoutofdesktopactivity = titoli delle finestre lasciati fuori dall'attività del desktop

linux.copy.yesthisismyjournal = sì, questo è il mio diario

linux.copy.your = tuo

linux.copy.yourcurrentpairingstaysinuseuntilyouconfirm = il tuo accoppiamento attuale resta in uso finché non confermi questa marca.

linux.copy.yourjournalnotsetupyet = il tuo diario, non ancora configurato

linux.copy.yourjournalsmarkisconfirmedbutthesolstoneap = la marca del tuo diario è confermata, ma non è stato possibile riavviare l'app di solstone. riprova.

linux.copy.yourjournalsmarkisconfirmedbutthesolstoneap2 = la marca del tuo diario è confermata, ma l'app di solstone richiede attenzione. controlla i dettagli, poi riprova.

linux.copy.yourjournalsmarkunavailablerightnow = la marca del tuo diario, non disponibile in questo momento

linux.copy.yoursettingsaresavedbutthesolstoneappcouldn = le tue impostazioni sono salvate, ma non è stato possibile riavviare l'app di solstone. riprova.

linux.copy.yoursettingsaresavedrestartthesolstoneappto = le tue impostazioni sono salvate. riavvia l'app di solstone per usarle ora.

linux.copy.yoursettingschangedoutsidethiswindowreadthem = le tue impostazioni sono cambiate fuori da questa finestra. leggile di nuovo prima di salvare.

linux.copy.yousaidthismarkdoesntmatchtheoneyourjourna2 = hai detto che questa marca non corrisponde a quella che mostra il tuo diario. il tuo accoppiamento attuale resta in uso. ottieni un nuovo link di accoppiamento dal tuo diario, oppure manda un'email a { $literal1 } e ti aiutiamo.

linux.desktop.clipboardcommandexitedunsuccessfully = il comando degli appunti è terminato senza successo

linux.desktop.couldnotopenyourjournalwaitforthesolstoneap = Non è stato possibile aprire il tuo diario. Aspetta che l'app di solstone si riconnetta, poi riprova.

linux.desktop.couldntcopyrunsolstonelinuxabouttoselectand = non è stato possibile copiare. esegui `{ $literal1 } about` per selezionare e copiare le tue versioni.

linux.desktop.couldntcopywithoutxinstallitorrunsolstonel = non è stato possibile copiare senza { $value1 }. installalo, oppure esegui `{ $literal1 } about` per selezionare e copiare le tue versioni.

linux.desktop.observercommandmustberoutedbytherunloopowne = il comando dell'osservatore deve essere instradato da chi possiede il run-loop

linux.doctor.cantreachyourdesktopxlogoutandbackintore = non è stato possibile connettersi al tuo desktop ({ $value1 }); esci e rientra per ripristinarlo

linux.doctor.couldnotreachthesessionbustocheckthepaneli = non è stato possibile connettersi al bus di sessione per controllare l'icona del pannello

linux.doctor.couldnotreadloginsessionsx = non è stato possibile leggere le sessioni di login: { $error }

linux.doctor.couldnotstarttheprobex = non è stato possibile avviare la sonda: { $error }

linux.doctor.displaynotsetstartsolstonelinuxfromthex11gr = { $literal1 } non impostata; avvia { $literal2 } dalla sessione grafica { $literal3 }

linux.doctor.doctor9checksxfailedxwarnings = { $ws1 }doctor: 9 controlli, { $failures } non riusciti, { $warnings } avvisi

linux.doctor.followingsessionxwayland = sessione seguita { $session } (wayland)

linux.doctor.followingsessionxx11onx = sessione seguita { $session } (x11 su { $display })

linux.doctor.gnomeneedsanextensionrunsolstonelinuxpanelic = { $literal1 } ha bisogno di un'estensione; esegui { $literal2 }

linux.doctor.gstreamerelementsxmissinginstallthepipewirea = { $literal1 } elementi { $value1 } mancanti; installa i pacchetti dei plugin { $literal2 } e good/base

linux.doctor.gstreamerinitializationfailedxinstallgstreamer = non è stato possibile inizializzare { $literal1 }: { $error }; installa { $literal1 } 1.x

linux.doctor.label.desktopsession = sessione del desktop

linux.doctor.label.fail = fallito

linux.doctor.label.gstreamer = gstreamer

linux.doctor.label.na = n.a.

linux.doctor.label.ok = ok

linux.doctor.label.panelicon = icona del pannello

linux.doctor.label.portal = xdg-desktop-portal

linux.doctor.label.pulse = pipewire (pulse)

linux.doctor.label.sessiontype = tipo di sessione

linux.doctor.label.synchealth = stato della sincronizzazione

linux.doctor.label.systemduser = systemd --user

linux.doctor.label.warn = avviso

linux.doctor.label.x11capture = cattura x11

linux.doctor.nodesktopsessionofyoursonthis = nessuna tua sessione del desktop su questo computer; schermo e audio ne hanno bisogno

linux.doctor.nodesktopsessionofyoursonthiscomputernothing = nessuna tua sessione del desktop su questo computer; schermo e audio aspettano che tu acceda qui (una sessione a cui accedi da remoto non conta)

linux.doctor.notapplicablenognomeshellonthesessionbus = non applicabile (nessuna { $literal1 } Shell sul bus di sessione)

linux.doctor.notapplicablenox11display = non applicabile (nessun display { $literal1 })

linux.doctor.notapplicablewaylandsession = non applicabile (sessione wayland)

linux.doctor.notregisterednotneededonx11usingximagesrc = non registrato — non necessario su { $literal1 } (si usa { $literal2 })

linux.doctor.okbrowserregistrationchromechromiumedgeandfir = ok{ $ws1 }registrazione del browser{ $ws2 }chrome, chromium, edge e firefox

linux.doctor.okfirefoxsnapallowedtostartthesolstoneapp = ok{ $ws1 }firefox (snap){ $ws2 }autorizzato ad avviare l'app di solstone

linux.doctor.okfirefoxsnapasksyouoncetoletitstarttheso = ok{ $ws1 }firefox (snap){ $ws2 }ti chiede una volta di lasciargli avviare l'app di solstone

linux.doctor.orgfreedesktopportaldesktopnotregisteredon = { $literal1 } non registrato sul bus di sessione; installa/avvia { $literal2 }

linux.doctor.orgfreedesktopportaldesktopregisteredonsession = { $literal1 } registrato sul bus di sessione

linux.doctor.pipewiresrcvp8encandwebmmuxavailable = pipewiresrc, vp8enc e webmmux disponibili

linux.doctor.probepanickedinspectservicelogsandretry = la sonda è andata in panic; ispeziona i registri del servizio e riprova

linux.doctor.pulseaudiocompatibleserverreachable = server compatibile con { $literal1 } raggiungibile

linux.doctor.pulseaudiocompatibleserverunreachablexstartpip = server compatibile con { $literal1 } non raggiungibile: { $error }; avvia { $literal2 } Pulse o { $literal1 }

linux.doctor.screenaudioanddesktopactivityareoff = schermo, audio e attività del desktop sono disattivi

linux.doctor.sessionbusprobefailedxstartagraphicalsession = non è stato possibile eseguire la sonda del bus di sessione: { $error }; avvia una sessione grafica e assicurati che { $literal1 } sia installato

linux.doctor.sessionbusunreachablexstartagraphicalsession = bus di sessione non raggiungibile: { $error }; avvia una sessione grafica e assicurati che { $literal1 } sia installato

linux.doctor.systemctlusernotreachableruninsideasystemdus = { $literal1 } non raggiungibile; esegui dentro una sessione utente di systemd

linux.doctor.systemctlusernotreachablexruninsideasystemd = { $literal1 } non raggiungibile: { $error }; esegui dentro una sessione utente di systemd

linux.doctor.thegnomeextensionisinstalledbutoffrunsolston = l'estensione di { $literal1 } è installata ma disattiva; esegui { $literal2 }

linux.doctor.thegnomeextensionisonbutnopaneliconyetlog = l'estensione di { $literal1 } è attiva ma ancora nessuna icona del pannello; esci e rientra se resta così

linux.doctor.thepaneliconextensionisinstalledlogoutandba = l'estensione dell'icona del pannello è installata; esci e rientra per finire

linux.doctor.thisdesktopcanshowthepanelicon = questo desktop può mostrare l'icona del pannello

linux.doctor.timedoutafter2schecktheuserdbussessionand = tempo scaduto dopo 2s; controlla la sessione D-Bus utente e { $literal1 }

linux.doctor.unrecognizedsessiontypexwaylandorx11required = tipo di sessione non riconosciuto '{ $value }'; { $literal1 } o { $literal2 } richiesti

linux.doctor.warnbrowserregistrationmissingoroutofdatefor = avviso{ $ws1 }registrazione del browser{ $ws2 }mancante o non aggiornata per { $value1 }; avvia l'app di solstone per riscriverle

linux.doctor.warnchromiumsnapcantreachthesolstoneappasn = avviso{ $ws1 }chromium (snap){ $ws2 }non è stato possibile connettersi all'app di solstone; uno snap di chromium non ha modo di avviarla. usa chrome, edge o firefox

linux.doctor.warnfirefoxsnapasksyouoncetoletitstartthe = avviso{ $ws1 }firefox (snap){ $ws2 }ti chiede una volta di lasciargli avviare l'app di solstone; per doctor non è stato possibile leggere che cosa hai risposto

linux.doctor.warnfirefoxsnapyoutoldyourdesktopnottoletf = avviso{ $ws1 }firefox (snap){ $ws2 }hai detto al tuo desktop di non lasciare che firefox avvii l'app di solstone, quindi da firefox non viene preso niente. perché ti venga chiesto di nuovo, esegui:{ $ws3 }gdbus { $value1 } --method { $literal4 } { $PERMISSION-TABLE } { $value2 } { $SNAP-FIREFOX }

linux.doctor.warnxcantreachthesolstoneappfrominsideafl = avviso{ $ws1 }{ $id }{ $ws1 }non è stato possibile connettersi all'app di solstone da dentro un flatpak; usa chrome, edge o firefox installati fuori da flatpak

linux.doctor.wayland = wayland

linux.doctor.x11randrandximagesrcavailable = { $literal1 } RandR e { $literal2 } disponibili

linux.doctor.x11randrunavailablexcheckdisplayandxserverr = { $literal1 } RandR non disponibile: { $error }; controlla { $literal2 } e il supporto RandR del server X

linux.doctor.x11usingximagesrccapture = x11 (usando la cattura { $literal1 })

linux.doctor.xcheckthatsystemdlogindisrunning = { $value1 }; controlla che { $literal1 } sia in esecuzione

linux.doctor.xdgsessiontypenotsetwaylandor = { $literal1 } non impostata; { $literal2 } o { $literal3 } richiesti

linux.doctor.ximagesrcmissinginstallthegstreamergoodplugins = { $literal1 } mancante; installa il pacchetto dei plugin good di { $literal2 }

linux.doctor.yourdesktopsessionisnttheoneinfrontofthes = la tua sessione del desktop non è quella davanti allo schermo; schermo e audio aspettano che sia quella

linux.health.idleconfirmingwithyourjournal = inattivo, conferma con il tuo diario

linux.health.idleconnected = inattivo, connesso

linux.health.idleconnecting = inattivo, connessione in corso

linux.health.idleconnectionunavailableheldonthisdevice = inattivo, connessione non disponibile (trattenuto su questo dispositivo)

linux.health.idleconnectionunavailableheldonthisdevice2 = inattivo, connessione non disponibile, trattenuto su questo dispositivo

linux.health.idlenostatus = inattivo, nessuno stato

linux.health.idleofflineheldonthisdevice = inattivo, non in linea (trattenuto su questo dispositivo)

linux.health.idleofflineheldonthisdevice2 = inattivo, non in linea, trattenuto su questo dispositivo

linux.health.idlepairagain = inattivo, accoppia di nuovo

linux.health.idlepairingnotsaved = inattivo, accoppiamento non salvato

linux.health.idlepairingrequired = inattivo, accoppiamento richiesto

linux.health.idlepairingunsafe = inattivo, accoppiamento non sicuro

linux.health.idlesyncing = inattivo, in sincronizzazione

linux.health.idlesyncuptodate = inattivo, sincronizzazione aggiornata

linux.health.idleupdaterequired = inattivo, aggiornamento richiesto

linux.health.journalmark = il tuo diario:{ $ws1 }{ $paired }

linux.health.markclaim = { $spoken }{ $ws1 }({ $claim })

linux.health.markclaimed = dichiarato, non verificato

linux.health.n0pending = 0 in attesa

linux.health.noidentitypresented = nessuna identità presentata

linux.health.onconfirmingwithyourjournal = attivo, conferma con il tuo diario

linux.health.onconnected = attivo, connesso

linux.health.onconnecting = attivo, connessione in corso

linux.health.onconnectionunavailableheldonthisdevice = attivo, connessione non disponibile (trattenuto su questo dispositivo)

linux.health.onconnectionunavailableheldonthisdevice2 = attivo, connessione non disponibile, trattenuto su questo dispositivo

linux.health.onnostatus = attivo, nessuno stato

linux.health.onofflineheldonthisdevice = attivo, non in linea (trattenuto su questo dispositivo)

linux.health.onofflineheldonthisdevice2 = attivo, non in linea, trattenuto su questo dispositivo

linux.health.onpairagain = attivo, accoppia di nuovo

linux.health.onpairingnotsaved = attivo, accoppiamento non salvato

linux.health.onpairingrequired = attivo, accoppiamento richiesto

linux.health.onpairingunsafe = attivo, accoppiamento non sicuro

linux.health.onsyncing = attivo, in sincronizzazione

linux.health.onsyncuptodate = attivo, sincronizzazione aggiornata

linux.health.onupdaterequired = attivo, aggiornamento richiesto

linux.health.pendingunconfirmed = in attesa, non confermato

linux.health.sighting = { $header }:{ $ws1 }che cosa ha risposto:{ $ws2 }{ $answer }{ $ws3 }{ $paired }{ $footer }

linux.health.syncconfirmingwithyourjournal = sincronizzazione: conferma con il tuo diario

linux.health.syncconnecteduptodate = sincronizzazione: connesso; aggiornato

linux.health.syncconnecting = sincronizzazione: connessione in corso

linux.health.syncconnectingwaitwhilethisdeviceconnectstoy = sincronizzazione: connessione in corso; aspetta mentre questo dispositivo si connette al tuo diario

linux.health.syncconnectionunavailableheldonthisdevice = sincronizzazione: connessione non disponibile; trattenuto su questo dispositivo

linux.health.syncconnectionunavailableheldonthisdevicerest = sincronizzazione: connessione non disponibile; trattenuto su questo dispositivo; riavvia l'app di solstone; se continua, accoppia di nuovo questo dispositivo

linux.health.syncconnectionunavailablerestartthesolstoneapp = sincronizzazione: connessione non disponibile; riavvia l'app di solstone; se continua, accoppia di nuovo questo dispositivo

linux.health.synchealthconnectingwaitwhilethisdeviceconnec = stato della sincronizzazione: connessione in corso; aspetta mentre questo dispositivo si connette al tuo diario

linux.health.synchealthconnectionunavailablerestartthesolst = stato della sincronizzazione: connessione non disponibile; riavvia l'app di solstone; se continua, accoppia di nuovo questo dispositivo

linux.health.synchealthofflineheldonthisdevicewillretry = stato della sincronizzazione: non in linea; trattenuto su questo dispositivo; riproverà

linux.health.synchealthpairingnotsavedfixthisdevicespair = stato della sincronizzazione: accoppiamento non salvato; sistema i permessi di accoppiamento di questo dispositivo, poi riavvia l'app di solstone

linux.health.synchealthpairingunsaferepairthisdevicespair = stato della sincronizzazione: accoppiamento non sicuro; ripara l'accoppiamento di questo dispositivo e riavvia l'app di solstone

linux.health.synchealthpairthisdevicewithyourjournal = stato della sincronizzazione: accoppia questo dispositivo con il tuo diario

linux.health.synchealthpairthisdevicewithyourjournalagain = stato della sincronizzazione: accoppia di nuovo questo dispositivo con il tuo diario

linux.health.synchealthupdaterequiredupdatethesolstoneapp = stato della sincronizzazione: aggiornamento richiesto; aggiorna l'app di solstone

linux.health.synchealthwaitwhilethisdeviceconfirmswithyou = stato della sincronizzazione: aspetta mentre questo dispositivo conferma con il tuo diario

linux.health.synchealthyourjournalisreceivingnotconfirmed = stato della sincronizzazione: in sincronizzazione; non ancora confermato dal tuo diario

linux.health.syncingfallback = in sincronizzazione...

linux.health.syncnostatus = sincronizzazione: nessuno stato

linux.health.syncnostatusfromthesolstoneapprightnow = sincronizzazione: nessuno stato dall'app di solstone in questo momento

linux.health.syncnostatusthesolstoneappisnotrunningorha = sincronizzazione: nessuno stato; l'app di solstone non è in esecuzione, o non ha ancora uno stato

linux.health.syncofflineheldonthisdevicewillretry = sincronizzazione: non in linea; trattenuto su questo dispositivo; riproverà

linux.health.syncofflinewillretry = sincronizzazione: non in linea; riproverà

linux.health.syncpairagain = sincronizzazione: accoppia di nuovo

linux.health.syncpairagainpairthisdevicewithyourjournala = sincronizzazione: accoppia di nuovo; accoppia di nuovo questo dispositivo con il tuo diario

linux.health.syncpairingnotsaved = sincronizzazione: accoppiamento non salvato

linux.health.syncpairingnotsavedfixthisdevicespairingper = sincronizzazione: accoppiamento non salvato; sistema i permessi di accoppiamento di questo dispositivo, poi riavvia l'app di solstone

linux.health.syncpairingrequired = sincronizzazione: accoppiamento richiesto

linux.health.syncpairingrequiredpairthisdevicewithyourjou = sincronizzazione: accoppiamento richiesto; accoppia questo dispositivo con il tuo diario

linux.health.syncpairingunsafe = sincronizzazione: accoppiamento non sicuro

linux.health.syncpairingunsaferepairthisdevicespairingand = sincronizzazione: accoppiamento non sicuro; ripara l'accoppiamento di questo dispositivo e riavvia l'app di solstone

linux.health.syncpairthisdevicewithyourjournal = sincronizzazione: accoppia questo dispositivo con il tuo diario

linux.health.syncpairthisdevicewithyourjournalagain = sincronizzazione: accoppia di nuovo questo dispositivo con il tuo diario

linux.health.syncprogress = sincronizzazione: { $progress }

linux.health.syncsyncingyourjournalisreceivingnotconfirmed = sincronizzazione: in sincronizzazione; non ancora confermato dal tuo diario

linux.health.syncupdaterequiredupdatethesolstoneapp = sincronizzazione: aggiornamento richiesto; aggiorna l'app di solstone

linux.health.syncupdatethesolstoneapp = sincronizzazione: aggiorna l'app di solstone

linux.health.syncuptodate = sincronizzazione: aggiornato

linux.health.syncwaitwhilethisdeviceconfirmswithyourjourn = sincronizzazione: aspetta mentre questo dispositivo conferma con il tuo diario

linux.health.syncwaitwhilethisdeviceconnectstoyourjournal = sincronizzazione: aspetta mentre questo dispositivo si connette al tuo diario

linux.health.thesolstoneappisnotrunningorhasnostatusyet = l'app di solstone non è in esecuzione, o non ha ancora uno stato

linux.health.unknownjournalseenatx = diario sconosciuto visto a { $address }

linux.health.unknownjournalseenthroughtherelay = diario sconosciuto visto attraverso il relay

linux.health.uptodateat = stato della sincronizzazione: aggiornato a { $timestamp }

linux.health.waitingforyoutoconfirmyourjournalsmarknothi = in attesa che tu confermi la marca del tuo diario. niente di ciò che è in attesa arriva nel tuo diario finché non lo fai.{ $ws1 }quando puoi, esegui: { $literal2 }

linux.import.allhistory = tutta la cronologia delle importazioni nel tuo diario

linux.import.attention = richiede attenzione

linux.import.cannotconvert = questo dispositivo non può convertire questa foto

linux.import.delivered = nel tuo diario.

linux.import.expired = scaduta

linux.import.explanation = aggiunge qualsiasi file che scegli qui o rilasci su questa finestra. il tuo diario capisce che cos'è.

linux.import.explanationnodrop = aggiunge qualsiasi file che scegli qui. il tuo diario capisce che cos'è.

linux.import.filekind = il tuo diario non può prendere questo tipo di file

linux.import.filemissing = manca la copia su questo dispositivo

linux.import.filenotread = non è stato possibile leggere questo. aggiungilo di nuovo

linux.import.filetoolarge = è più grande di quanto il tuo diario prende

linux.import.held = non vanno nel tuo diario ora. ognuno dice perché.

linux.import.inyourjournalrel = nel tuo diario · { $when }

linux.import.journalrefusal = non è stato possibile accettare questo nel tuo diario.

linux.import.moreimports = altre importazioni

linux.import.moreimportssub = guide, opzioni e cronologia delle importazioni nel tuo diario

linux.import.namedrefusal = { $name }: { $reason }

linux.import.nfiles = { $count } file

linux.import.noroom = non c'è abbastanza spazio su questo dispositivo

linux.import.nothingwassaved = non è stato salvato niente su questo dispositivo.

linux.import.nothingyet = ancora niente.

linux.import.nreadyinjournal = { $count } pronti nel tuo diario

linux.import.nwaiting = { $count } in attesa

linux.import.pairagain = il tuo diario ha smesso di accettare questo dispositivo. le importazioni restano in attesa qui finché non accoppi di nuovo.

linux.import.readytoreview = pronto per rivedere

linux.import.readytoreviewcount = { $count } pronte per rivedere

linux.import.recentrow.a11y = { $name }, nel tuo diario, { $when }. apri nel tuo diario.

linux.import.remove.a11y = rimuovi { $name }

linux.import.removeerror = non è stato possibile rimuovere questo. riprova.

linux.import.removereadytoreview = questo rimuove la copia su questo dispositivo. la copia in attesa nel tuo diario resta lì finché non la scarti o non scade.

linux.import.reviewdetail = salvato nel tuo diario. mostra l'anteprima e conferma per iniziare.

linux.import.reviewdetailhour = salvato nel tuo diario. rivedi entro un'ora prima che il tuo diario rimuova la copia in attesa.

linux.import.reviewexpireddetail = la copia in attesa nel tuo diario è scaduta. questo dispositivo tiene ancora il file.

linux.import.reviewinjournal = rivedi nel tuo diario

linux.import.reviewinjournal.a11y = rivedi { $name } nel tuo diario

linux.import.saved = salvato su questo dispositivo: { $count }

linux.import.saving = salvataggio su questo dispositivo: { $saved } di { $total }

linux.import.sendagain = invia di nuovo per rivedere

linux.import.sendagain.a11y = invia di nuovo { $name } per rivedere

linux.import.sending = in invio

linux.import.sendingellipsis = in invio al tuo diario…

linux.import.sendingtojournal = in invio al tuo diario.

linux.import.statusneedsyou = le importazioni hanno bisogno di te

linux.import.statusneedsyousub = apri l'importazione per vedere perché.

linux.import.statusreadytoreview = { $count ->
    [one] { $display } importazione pronta per rivedere
    [many] { $display } di importazioni pronte per rivedere
   *[other] { $display } importazioni pronte per rivedere
    }

linux.import.statusreadytoreviewsub = apri l'importazione per rivedere e iniziare.

linux.import.statusrow.a11y = importazione, { $detail }.

linux.import.steperror = non è stato possibile finire questo passaggio. riprova.

linux.import.storeerror = non è stato possibile leggere le importazioni su questo dispositivo. non è stato rimosso niente. riavvia l'app di solstone per riprovare.

linux.import.tile.a11y = importazione, { $state }. { $detail }.

linux.import.trynow.a11y = prova { $name } ora

linux.import.waitforjournal = le importazioni restano in attesa su questo dispositivo finché non connetti un diario.

linux.import.waitformark = le importazioni restano in attesa finché non confermi la marca del tuo diario.

linux.import.waitforupdate = le importazioni restano in attesa finché il tuo diario non è aggiornato.

linux.import.waiting = in attesa

linux.import.waitingdetail = in attesa di andare nel tuo diario

linux.import.waitingforjournal = in attesa di andare nel tuo diario.

linux.import.waitingforjournalshort = in attesa del tuo diario

linux.import.waitingforyourreview = in attesa della tua revisione

linux.import.waitingreconnect = in attesa che il tuo diario si riconnetta.

linux.journalmark.journal = diario: { $value1 }·{ $value2 }

linux.journalmark.journaljournal = diario: il tuo diario

linux.journalmark.journalnotpaired = diario: non accoppiato

linux.journalmark.journalsmarkunavailableright = marca del tuo diario: non disponibile ora

linux.journalmark.markwordsyougavedon = le parole della marca che hai dato non corrispondono alla marca del diario, quindi questo computer non è accoppiato. controlla le parole, ottieni un nuovo link di accoppiamento dal tuo diario e accoppia di nuovo.

linux.journalmark.markwordsyougavedon2 = le parole della marca che hai dato non corrispondono alla marca del diario.

linux.journalmark.notchangedcurrentpairingstays = non è cambiato. l'accoppiamento attuale resta in uso. niente di ciò che questo computer ha conservato è andato a quel diario tramite questo link.

linux.journalmark.notpairedgetfreshpairing = non accoppiato. ottieni un nuovo link di accoppiamento dal tuo diario e accoppia questo computer.

linux.journalmark.pairingcancelledcurrentpairingstays = accoppiamento annullato. l'accoppiamento attuale resta in uso.

linux.journalmark.pairingcancelledgetfreshpairing = accoppiamento annullato. ottieni un nuovo link di accoppiamento dal tuo diario e accoppia di nuovo quando sei pronto.

linux.journalmark.pastepairinglinkfromjournal = incolla il link di accoppiamento dal tuo diario, poi premi invio:{ $ws1 }

linux.journalmark.setupfailedpairinglinkwas = configurazione iniziale non riuscita: il link di accoppiamento non era valido.

linux.journalmark.yousaidthismarkdoesn = hai detto che questa marca non corrisponde a quella che mostra il tuo diario, quindi questo computer non è accoppiato. potresti aver incollato il link sbagliato, o qualcosa non va. ottieni un nuovo link di accoppiamento dal tuo diario e accoppia di nuovo, oppure scrivi a { $literal1 } e ti aiuteremo.

linux.metadata.autostartcomment = l'app di solstone prende ciò che condividi con lei, e tutto va nel tuo diario

linux.metadata.autostartname = app di solstone

linux.metadata.gnomedescription = ausiliario di attività desktop per { $command }

linux.metadata.gnomename = ausiliario di attività di solstone

linux.metadata.kwindescription = Ausiliario di attività di KWin per { $command }

linux.metadata.kwinname = ausiliario di attività di solstone

linux.metadata.name = solstone

linux.metadata.packagedescription = l'app di solstone per linux. prende lo schermo e l'audio che condividi con lei, e tutto va nel tuo diario.

linux.metadata.packagesummary = l'app di solstone per linux: ciò che condividi va nel tuo diario

linux.metadata.service = app di solstone per linux

linux.notification.copied = copiato

linux.notification.screenelementsmissing = l'ingresso dello schermo non può iniziare: { $literal1 } non ha { $elements }. installa i plugin { $literal2 }, good e base di { $literal1 } con il tuo gestore di pacchetti, poi scegli riprova nella pagina dello schermo.

linux.notification.screenelementsmissingx11 = l'ingresso dello schermo non può iniziare: { $literal1 } non ha { $elements }. installa i plugin good e base di { $literal1 } con il tuo gestore di pacchetti, poi scegli riprova nella pagina dello schermo.

linux.pair.cancel = annulla

linux.pair.continue = continua

linux.pair.onemorestepcheckyourjournalsmarkyourjournal = ancora un passaggio: controlla la marca del tuo diario.{ $ws1 }{ $ws2 }marca del tuo diario: { $spoken }{ $ws1 }{ $ws1 }il tuo diario mostra questa stessa marca nella sua app di rete. deve corrispondere, esattamente.{ $ws1 }corrisponde al tuo diario? digita { $literal3 } o { $literal4 }:{ $ws1 }

linux.pair.onemorestepcheckyourjournalsmarkyourjournal2 = ancora un passaggio: controlla la marca del tuo diario.{ $ws1 }{ $ws2 }marca del tuo diario: non disponibile ora{ $ws1 }{ $ws1 }non è stato possibile verificare.{ $ws1 }questo computer non ha potuto determinare la marca del tuo diario, quindi non c'è niente da confrontare. { $literal3 } solo se hai la certezza che il link provenga dal tuo diario.{ $ws1 }digita { $literal3 } per accoppiare comunque, o { $literal4 } per fermarti:{ $ws1 }

linux.pair.pairingcancellednothingthiscomputerhaskeptwen = accoppiamento annullato. niente di ciò che questo computer ha conservato è andato a quel diario tramite questo link. ottieni un nuovo link di accoppiamento dal tuo diario ed esegui di nuovo la configurazione iniziale quando sei pronto.

linux.pair.themarkwordsyougavedontmatchthejournalsma = le parole della marca che hai dato non corrispondono alla marca del diario, quindi questo computer non è accoppiato, e niente di ciò che ha conservato è andato a quel diario tramite questo link. controlla le parole, ottieni un nuovo link di accoppiamento dal tuo diario ed esegui di nuovo la configurazione iniziale.

linux.pair.thiscomputercouldntworkoutthejournalsmarks = questo computer non ha potuto determinare la marca del diario, quindi le parole che hai dato non si possono confrontare. questo computer non è accoppiato, e niente di ciò che ha conservato è andato a quel diario tramite questo link. ottieni un nuovo link di accoppiamento dal tuo diario ed esegui la configurazione iniziale in un terminale per decidere da te.

linux.pair.thiscomputercouldntworkoutthejournalsmarks2 = questo computer non ha potuto determinare la marca del diario, quindi le parole che hai dato non si possono confrontare. in attesa che tu confermi la marca del tuo diario. niente di ciò che è in attesa va nel tuo diario finché non lo fai. esegui { $literal1 } in un terminale per decidere da te.

linux.pair.whenyourereadyrunsolstonelinuxconfirm = quando sei pronto, esegui: { $literal1 }

linux.pair.yousaidthismarkdoesntmatchtheoneyourjourna = hai detto che questa marca non corrisponde a quella che mostra il tuo diario, quindi questo computer non è accoppiato, e niente di ciò che ha conservato è andato a quel diario tramite questo link. potresti aver incollato il link sbagliato, o qualcosa non va. ottieni un nuovo link di accoppiamento dal tuo diario ed esegui di nuovo la configurazione iniziale, oppure scrivi a { $literal1 } e ti aiuteremo.

linux.panel.couldnotreachthedesktoptocheckwhatitneeds = non è stato possibile connettersi al desktop per controllare che cosa gli serve

linux.panel.couldnotreachthesolstoneappx = non è stato possibile connettersi all'app di solstone: { $reason }

linux.panel.couldnotreachthisdesktoptofindoutwhatitnee = non è stato possibile connettersi a questo desktop per scoprire che cosa gli serve.

linux.panel.donethepaneliconisinyourpanelnow = fatto. l'icona del pannello è nel tuo pannello ora.

linux.panel.dontaskagain = non chiedere di nuovo

linux.panel.erroractivityhelperassetsdirectorynotfound = errore: directory delle risorse dell'ausiliario di attività non trovata

linux.panel.errorfailedtocopygnomeactivityhelperx = errore: non è stato possibile copiare l'ausiliario di attività { $literal1 }: { $err }

linux.panel.errorfailedtocopykwinactivityhelperx = errore: non è stato possibile copiare l'ausiliario di attività { $literal1 }: { $err }

linux.panel.errornoactivityhelperassetsfoundinx = errore: nessuna risorsa dell'ausiliario di attività trovata in { $source }

linux.panel.gnomegaveananswerwedonotknowx = { $literal1 } ha dato una risposta che non conosciamo: { $other }

linux.panel.gnomeinstalledtheextensionbutcouldnotturnit = { $literal1 } ha installato l'estensione ma non ha potuto attivarla

linux.panel.gnomeissetnottoloadextensionssothepanelico = { $literal1 } è impostato per non caricare le estensioni, quindi l'icona del pannello non può tornare in questo modo

linux.panel.gnomeneedsoneextensionbeforethepaneliconcan = { $literal1 } ha bisogno di un'estensione prima che l'icona del pannello possa apparire. quell'icona è dove si trovano pausa e riprendi.

linux.panel.gnometooktheextensionbutthepaneliconhasnot = { $literal1 } ha preso l'estensione ma l'icona del pannello non è apparsa. esegui { $literal2 } doctor per vedere dove è arrivata.

linux.panel.gnometooktheextensionbutthepaneliconhasnot2 = { $literal1 } ha preso l'estensione ma l'icona del pannello non è ancora apparsa. dagli un momento, poi esegui: { $literal2 } doctor

linux.panel.gnomewillaskyoutoconfirmacceptittoaddthep = { $literal1 } ti chiederà di confermare. accettala per aggiungere l'estensione dell'icona del pannello.

linux.panel.gnomewouldnotturntheextensionon = { $literal1 } non ha voluto attivare l'estensione

linux.panel.installedactivityhelperassetstouserdatadirect = installate le risorse dell'ausiliario di attività nelle directory dei dati utente.

linux.panel.logoutandbackinthenrunthisagain = esci e rientra, poi esegui di nuovo questo.

linux.panel.nochangeyoucanrunthisagainanytime = nessuna modifica. puoi eseguire questo di nuovo in qualsiasi momento.

linux.panel.noinstalledappindicatorextensiontoturnon = nessuna estensione appindicator installata da attivare

linux.panel.noteyoumustexplicitlyenabletheextensionorscr = nota: devi attivare esplicitamente l'estensione o lo script nella tua sessione desktop.

linux.panel.nothingherecansetupapanelicononthisdesktop = niente qui può configurare un'icona del pannello su questo desktop.

linux.panel.nothingtodo = niente da fare.

linux.panel.nothingtosetuphere = niente da configurare qui.

linux.panel.notnow = non ora

linux.panel.notrunning = l'app di solstone non è in esecuzione, quindi non c'è niente da mettere in pausa o riprendere.

linux.panel.pausedforxminutesrunsolstonelinuxresumetoe = in pausa per { $minutes } minuti. esegui { $literal1 } resume per terminarla prima.

linux.panel.pausedrunsolstonelinuxresumewhenyouwantitba = in pausa. esegui { $literal1 } resume quando la rivuoi.

linux.panel.setitup = configurala

linux.panel.solstoneapp = app di solstone

linux.panel.thedownloadedextensioncouldnotbeunpacked = non è stato possibile estrarre l'estensione scaricata

linux.panel.theextensioncouldnotbedownloadedcheckthenetw = non è stato possibile scaricare l'estensione; controlla la connessione di rete

linux.panel.thegnomeextensionisalreadyonifthepanelicon = l'estensione { $literal1 } è già attiva. se l'icona del pannello manca ancora, esci e rientra.

linux.panel.thegnomeextensionisinstalledbutturnedoffturn = l'estensione { $literal1 } è installata ma disattivata. la attivo ora.

linux.panel.thepaneliconextensionisinstalledbutgnomehas = l'estensione dell'icona del pannello è installata, ma { $literal1 } non l'ha ancora rilevata.

linux.panel.thepaneliconextensionisinstalledlogoutandba = l'estensione dell'icona del pannello è installata. esci e rientra, poi esegui di nuovo { $literal1 }.

linux.panel.thepaneliconisalreadyavailableonthisdesktop = l'icona del pannello è già disponibile su questo desktop.

linux.panel.thepaneliconisnotsetupx = l'icona del pannello non è configurata: { $reason }

linux.panel.thepaneliconisreadypauseandresumeareinthe = l'icona del pannello è pronta. pausa e riprendi sono nel pannello ora.

linux.panel.thesolstoneappisnotrunningsothereisnothing = l'app di solstone non è in esecuzione, quindi non c'è niente da mettere in pausa o riprendere.

linux.panel.thisdesktopdoesnotallowextensionstobeinstall = questo desktop non consente di installare le estensioni

linux.panelicon.resumed = ripresa.

linux.screenshots.bringcount = { $count ->
    [one] porta dentro 1 screenshot
    [many] porta dentro { $formatted } di screenshot
   *[other] porta dentro { $formatted } screenshot
    }

linux.screenshots.refusedcount = { $count ->
    [one] 1 screenshot non può entrare, quindi resta nella cartella.
    [many] { $formatted } di screenshot non possono entrare, quindi restano nella cartella.
   *[other] { $formatted } screenshot non possono entrare, quindi restano nella cartella.
    }

linux.screenshots.total = { $count ->
    [one] 1 screenshot
    [many] { $formatted } di screenshot
   *[other] { $formatted } screenshot
    }

linux.screenshots.unreadablecount = { $count ->
    [one] non è stato possibile leggere 1 screenshot.
    [many] non è stato possibile leggere { $formatted } di screenshot.
   *[other] non è stato possibile leggere { $formatted } screenshot.
    }

linux.service.currentexecutablehasnoparentdirectory = l'eseguibile attuale non ha una directory superiore

linux.service.errorremovingxx = Errore nella rimozione di { $value1 }: { $error }

linux.service.errorwritingservicefilesx = Errore nella scrittura dei file di servizio: { $error }

linux.service.homeisnotsetcannotlocateuserservicefiles = HOME non è impostata; non è stato possibile trovare i file di servizio utente

linux.service.invalidservicepid = PID del servizio non valido

linux.service.removed = Ha rimosso { $value1 }

linux.service.servicepidunavailable = PID del servizio non disponibile

linux.service.servicerestartfailedx = Riavvio del servizio non riuscito: { $error }

linux.service.systemctlcouldnotrestartsolstonelinuxservicex = systemctl non ha potuto riavviare { $literal1 }.service:{ $ws2 }{ $value1 }

linux.service.systemctlnotfound = systemctl non trovato

linux.service.systemctlstopfailed = systemctl stop non riuscito

linux.service.warningcouldnotremoveerror = Avviso: non è stato possibile rimuovere { $value1 }: { $error }

linux.service.warningcouldnotremoveeverybrowserregistration = Avviso: non è stato possibile rimuovere tutte le registrazioni del browser: { $error }

linux.service.warningcouldnotwriteeverybrowserregistrationx = Avviso: non è stato possibile scrivere tutte le registrazioni del browser: { $error }

linux.service.warningsystemctlenablenowfailedrunsystemctlus = Avviso: systemctl enable --now non riuscito; esegui '{ $literal1 }' per esaminare l'errore

linux.service.warningsystemctlenablenowfailedxrunsystemctl = Avviso: systemctl enable --now non riuscito: { $error }; esegui '{ $literal1 }' dopo che systemd è disponibile

linux.service.warningsystemctlstartfailedrunsystemctluserst = Avviso: systemctl start non riuscito; esegui '{ $literal1 }' per esaminare l'errore

linux.service.warningsystemctlstartfailedxrunsystemctluser = Avviso: systemctl start non riuscito: { $error }; esegui '{ $literal1 }' dopo che systemd è disponibile

linux.service.warningsystemctlxfailedrunsystemctluserxtoi = Avviso: systemctl { $operation } non riuscito; esegui '{ $literal1 } { $operation }' per esaminare l'errore

linux.service.warningsystemctlxfailedxrunsystemctluserxaf = Avviso: systemctl { $operation } non riuscito: { $error }; esegui '{ $literal1 } { $operation }' dopo che systemd è disponibile

linux.service.wrotex = Ha scritto { $value1 }

linux.shared.cli.versionlastknown = { $value1 } (ultimo noto)

linux.shared.copy.connected = connesso

linux.shared.copy.status = stato

linux.shared.copy.yourjournalshowsthissamemarkinitsnetworkapp = il tuo diario mostra questa stessa marca nella sua app di rete. dovrebbe corrispondere, esattamente.

linux.shared.health.unknown = sconosciuto

linux.shared.pair.confirmcantaskyouaboutyourjournalsmarkhere = confirm non può chiederti della marca del tuo diario qui. eseguilo in un terminale, o dai le due parole della marca con --mark "word word".

linux.shared.pair.couldntverify = non è stato possibile verificare.

linux.shared.pair.doesthismatchyourjournaltypeyesorno = corrisponde al tuo diario? digita { $literal1 } o { $literal2 }:{ $ws3 }

linux.shared.pair.givethetwowordsofyourjournalsmarklikemark = dai le due parole della marca del tuo diario, come { $literal1 }.

linux.shared.pair.notpaired = non accoppiato.

linux.shared.pair.setupcantaskyouaboutyourjournalsmarkherer = la configurazione iniziale non può chiederti della marca del tuo diario qui. eseguila in un terminale, o dai le due parole della marca con --mark "word word".

linux.shared.pair.thetwowordsofthemarkyourjournalsnetworkapp = le due parole della marca che mostra l'app di rete del tuo diario. servono quando non c'è un terminale su cui chiederti.

linux.shared.pair.typecontinuetopairanywayorcanceltostop = digita { $literal1 } per accoppiare comunque, o { $literal2 } per fermarti:{ $ws3 }

linux.shared.pair.waitingforyoutoconfirmyourjournalsmarknothi = in attesa che tu confermi la marca del tuo diario. niente di ciò che è in attesa va nel tuo diario finché non lo fai.

linux.shared.pair.yourjournalsmarkisalreadyconfirmednothingto = la marca del tuo diario è già confermata. niente da fare.

linux.shared.tray.offline = non in linea

linux.sharedsources.also = anche su

linux.sharedsources.cli = Obsidian: { $state }

linux.sharedsources.many = da { $count } altri dispositivi

linux.sharedsources.one = da un altro dispositivo

linux.sharedsources.pair = { $first } · { $second }

linux.shell.activitystillon = l'attività è ancora attiva.

linux.shell.afternoon = buon pomeriggio

linux.shell.amountsince = { $total }, da { $date }

linux.shell.attention = richiede attenzione

linux.shell.browserunavailable = le pagine del browser non possono iniziare ora.

linux.shell.checking = controllo…

linux.shell.evening = buonasera

linux.shell.hourcount = { $count ->
    [one] { $display } ora
    [many] { $display } di ore
   *[other] { $display } ore
    }

linux.shell.hoursago = { $count ->
    [one] { $display } ora fa
    [many] { $display } di ore fa
   *[other] { $display } ore fa
    }

linux.shell.intakeunreadable = non è stato possibile leggere il registro di quanto è stato preso.

linux.shell.itemcount = { $count ->
    [one] { $display } elemento
    [many] { $display } di elementi
   *[other] { $display } elementi
    }

linux.shell.lessthanminute = meno di un minuto

linux.shell.lessthanminuteleft = in pausa, meno di un minuto rimasto

linux.shell.minutecount = { $count ->
    [one] { $display } minuto
    [many] { $display } di minuti
   *[other] { $display } minuti
    }

linux.shell.minutesago = { $minutes } min fa

linux.shell.month.april = aprile

linux.shell.month.august = agosto

linux.shell.month.december = dicembre

linux.shell.month.february = febbraio

linux.shell.month.january = gennaio

linux.shell.month.july = luglio

linux.shell.month.june = giugno

linux.shell.month.march = marzo

linux.shell.month.may = maggio

linux.shell.month.november = novembre

linux.shell.month.october = ottobre

linux.shell.month.september = settembre

linux.shell.monthday = { $month } { $day }

linux.shell.monthdayyear = { $month } { $day }, { $year }

linux.shell.morning = buongiorno

linux.shell.notecount = { $count ->
    [one] { $display } nota
    [many] { $display } di note
   *[other] { $display } note
    }

linux.shell.pagecount = { $count ->
    [one] { $display } pagina
    [many] { $display } di pagine
   *[other] { $display } pagine
    }

linux.shell.pauseleft = in pausa, { $minutes } min rimanenti

linux.shell.readytosetup = pronto per configurare

linux.shell.screenmissingplugin = manca un plugin di { $literal1 }.

linux.shell.screensharingnotallowed = la condivisione dello schermo non è stata consentita.

linux.shell.screenshotcount = { $count ->
    [one] { $display } screenshot
    [many] { $display } di screenshot
   *[other] { $display } screenshot
    }

linux.shell.screenstartfailed = non è stato possibile avviare l'ingresso dello schermo.

linux.shell.settingup = configurazione in corso

linux.shell.terminalunavailable = le sessioni del terminale non possono avviarsi in questo momento.

linux.shell.tileneedsattention = { $source } richiede attenzione

linux.shell.untilresume = in pausa finché non riprendi

linux.shell.withaudio = con audio

linux.shell.withaudioactivity = con audio e attività

linux.shell.yesterday = ieri

linux.state.stoppedbyanerrorrestarting = fermato da un errore, riavvio

linux.state.terminalisoff = il terminale è disattivo

linux.state.tmuxnotfound = { $literal1 } non si trova

linux.state.waitingsolstonefortmuxisstillinstalled = in attesa: solstone per { $literal1 } è ancora installato. attiva il terminale nell'app, oppure esegui `{ $literal2 } terminal on`, per spostarlo

linux.status.browsernobrowserconnected = browser: nessun browser connesso

linux.status.browserthesolstoneappisnotrunning = browser: l'app di solstone non è in esecuzione

linux.status.heldxsegmentsnotsent = Trattenuti: { $value1 } segmenti non inviati

linux.status.heldxsegmentsnotsentoldestxd = Trattenuti: { $value1 } segmenti non inviati, il più vecchio di { $value2 } gg

linux.status.pausedsononewpagesaretaken = { $ws1 }in pausa, quindi non vengono prese nuove pagine

linux.status.theroomforbrowserpagesonthis = { $ws1 }lo spazio per le pagine del browser su questo computer è pieno, quindi non vengono prese nuove pagine finché alcune non arrivano nel tuo diario

linux.sync.checkingjournal = controllo del diario...

linux.sync.checkingx = controllo di { $day }...

linux.sync.probefailednextinxs = { $count ->
    [one] non è stato possibile eseguire la sonda, la prossima tra { $count } secondo
    [many] non è stato possibile eseguire la sonda, la prossima tra { $count } di secondi
   *[other] non è stato possibile eseguire la sonda, la prossima tra { $count } secondi
    }

linux.sync.probingjournal = sonda del diario...

linux.sync.syncing = in sincronizzazione...

linux.sync.uploadingx = caricamento di { $segment-key }

linux.sync.xsuntilprobe = { $count ->
    [one] { $count } secondo alla sonda
    [many] { $count } di secondi alla sonda
   *[other] { $count } secondi alla sonda
    }

linux.synchealth.ifyourecentlyresetjournal = se hai reimpostato il tuo diario di recente, accoppia di nuovo questo dispositivo.

linux.synchealth.thisislesscertainthan = questo è meno certo di una connessione diretta.

linux.terminal.control.bringold = spostamento di ciò che è in attesa

linux.terminal.control.confirmationsolstoneappisnt = { $confirmation } l'app di solstone non è in esecuzione; questo ha effetto quando si avvia.

linux.terminal.control.couldnotmovesolstonetmux = non è stato possibile spostare dentro solstone per { $literal1 }: { $error }

linux.terminal.control.couldnotreachsolstoneapp = non è stato possibile connettersi all'app di solstone: { $error }

linux.terminal.control.couldnotrecordmoveyet = non è stato ancora possibile scrivere il registro dello spostamento: { $error }

linux.terminal.control.couldnotsavethaterror = non è stato possibile salvare quello: { $error }

linux.terminal.control.couldntuseproblem = { $ws1 }non è stato possibile usare { $value1 }: { $problem }

linux.terminal.control.ignoredsettingkey = { $ws1 }impostazione ignorata: { $key }

linux.terminal.control.isoff = il terminale è disattivo.

linux.terminal.control.ison = il terminale è attivo.

linux.terminal.control.morethanoneoldpairing = { $ws1 }più di un vecchio accoppiamento: { $value1 }

linux.terminal.control.moving = solstone per tmux si sta spostando dentro.

linux.terminal.control.movingsolstonetmuxneedsok = lo spostamento dentro di solstone per { $literal1 } ha bisogno del tuo ok. esegui questo in un terminale, oppure aggiungi --yes a `terminal adopt-{ $literal1 }`.

linux.terminal.control.movingsolstonetmuxphase = { $ws1 }solstone per { $literal2 } si sta spostando dentro: { $phase }

linux.terminal.control.newterminalhistoryfromhere = { $ws1 }nuova cronologia del terminale da qui, e ciò che è già nel tuo diario resta ({ $reason })

linux.terminal.control.nosolstonetmuxmove = non c'è solstone per { $literal1 } da spostare dentro.

linux.terminal.control.oldsegmentsheldback = { $ws1 }{ $value1 } vecchi segmenti trattenuti

linux.terminal.control.problem = { $ws1 }problema: { $value1 }

linux.terminal.control.recordmovingsolstonetmuxcan = il registro dello spostamento di solstone per { $literal1 } non si può leggere: { $value1 }. per favore contatta il supporto e non eliminarlo.

linux.terminal.control.screenaudiorestartappafter = schermo e audio: riavvia l'app dopo che entri nel tuo desktop qui

linux.terminal.control.screenisoff = schermo, audio e attività del desktop sono disattivi.

linux.terminal.control.screenison = schermo, audio e attività del desktop sono attivi.

linux.terminal.control.screenoffstatus = schermo, audio e attività del desktop: disattivi

linux.terminal.control.segmentsfromsolstonetmux = { $ws1 }{ $value1 } segmenti da solstone per { $literal2 } in attesa di caricamento

linux.terminal.control.segmentsheld = { $ws1 }{ $value1 } segmenti trattenuti

linux.terminal.control.segmentswaitingupload = { $ws1 }{ $value1 } segmenti in attesa di caricamento

linux.terminal.control.solstoneappdidnotanswer = l'app di solstone non ha risposto in tempo; riprova tra un momento

linux.terminal.control.solstoneappdidnotanswer2 = l'app di solstone non ha risposto, quindi non è cambiato niente; riprova tra un momento.

linux.terminal.control.solstoneappisstartingtry = l'app di solstone si sta avviando; riprova tra un momento.

linux.terminal.control.solstoneappisstillmoving = l'app di solstone sta ancora spostando dentro solstone per { $literal1 }; finisce da sola.

linux.terminal.control.solstoneappneedsrestartingbefore = l'app di solstone va riavviata prima di poter spostare dentro solstone per { $literal1 }.

linux.terminal.control.solstonetmuxhasalreadymoved = solstone per { $literal1 } è già stato spostato dentro.

linux.terminal.control.solstonetmuxhasmoved = solstone per { $literal1 } è stato spostato dentro.

linux.terminal.control.solstonetmuxhasmovedsolstone = solstone per { $literal1 } è stato spostato dentro. l'app di solstone completa il resto quando si avvia.

linux.terminal.control.solstonetmuxisstillinstalled = solstone per { $literal1 } è ancora installato su questo computer, quindi il terminale resta disattivo per ora

linux.terminal.control.stopold = arresto della vecchia app

linux.terminal.control.thismovessolstonetmuxinto = questo sposta solstone per { $literal1 } in questa app, e si ferma per sempre. ciò che ha ancora in attesa arriva nel tuo diario attraverso questa app, e ciò che non può arrivare resta su questo computer. se questa app non è ancora accoppiata, si accoppia con il diario che usa solstone per { $literal1 }. lo sposti ora? [y/N]

linux.terminal.control.tmux = { $ws1 }{ $literal2 }: { $value1 }

linux.terminal.control.unfinishedsegmentswaitingterminal = { $ws1 }{ $value1 } segmenti non finiti in attesa che il terminale torni in esecuzione

linux.terminal.control.waitinganswer = { $ws1 }in attesa: { $answer }

linux.terminal.malformedcommand = comando malformato

linux.terminal.state.stoppedbyerrorrestartingerror = fermato da un errore, riavvio: { $error }

linux.terminal.takeover.couldnotremoveerror = non è stato possibile rimuovere { $value1 }: { $error }

linux.terminal.takeover.isnotsolstonetmuxservice = { $value1 } non è il servizio di solstone per { $literal1 }, quindi non è cambiato niente.

linux.terminal.takeoverrunner.couldnotsaveterminalsettings = non è stato possibile salvare le impostazioni del terminale: { $error }

linux.terminal.takeoverrunner.couldnotturnterminalerror = non è stato possibile attivare il terminale: { $error }

linux.terminal.warning = avviso: { $detail }

linux.tray.aboutsolstone = informazioni su solstone

linux.tray.cachexmb = cache: { $value1 } MB

linux.tray.cantcheckwhichsessionisinfront = non è stato possibile controllare quale sessione è in primo piano

linux.tray.cantreachyourdesktop = non è stato possibile connettersi al tuo desktop

linux.tray.connectedallcaughtup = connesso · tutto in pari

linux.tray.idlescreeninactive = inattivo (schermo non attivo)

linux.tray.n15minutes = 15 minuti

linux.tray.n1hour = 1 ora

linux.tray.n30minutes = 30 minuti

linux.tray.notpaired = non accoppiato

linux.tray.notrunning = non in esecuzione

linux.tray.offlinewaiting = non in linea · { $count } in attesa

linux.tray.openjournal = apri il diario

linux.tray.opensettings = apri le impostazioni

linux.tray.pausedxmremaining = { $count ->
    [one] in pausa ({ $count } minuto rimanente)
    [many] in pausa ({ $count } di minuti rimanenti)
   *[other] in pausa ({ $count } minuti rimanenti)
    }

linux.tray.quitsolstone = esci da solstone

linux.tray.resume = riprendi

linux.tray.resumexmremaining = { $count ->
    [one] riprendi ({ $count } minuto rimanente)
    [many] riprendi ({ $count } di minuti rimanenti)
   *[other] riprendi ({ $count } minuti rimanenti)
    }

linux.tray.screenintakecantstart = l'ingresso dello schermo non può avviarsi

linux.tray.screenintakecouldntstart = non è stato possibile avviare l'ingresso dello schermo

linux.tray.screenneedsattention = lo schermo richiede attenzione

linux.tray.screensharingnotallowed = la condivisione dello schermo non è stata consentita

linux.tray.tooltip = { $status }{ $break }{ $health }

linux.tray.untiliresume = finché non riprendo

linux.tray.waitingforscreenintake = in attesa dell'ingresso dello schermo

linux.tray.waitingforyourdesktop = in attesa del tuo desktop

linux.tray.waitingnoneofyoursessionsisinfront = in attesa (nessuna delle tue sessioni è in primo piano)

linux.tray.waitingyoursessionisntinfront = in attesa (la tua sessione non è in primo piano)

linux.ui.terminalterminalstatus = terminale: { $terminal-status }

linux.window.addmore = aggiungi altro

linux.window.allcaughtup = tutto in pari

linux.window.brightharbor = bright · harbor

linux.window.browsers = browser

linux.window.browsersbreath = sui siti che consenti, l'estensione di solstone prende il testo e la disposizione approssimativa della pagina, e tutto va nel tuo diario.

linux.window.browsersgap = le pagine arrivano dall'estensione di solstone. qui non c'è un interruttore per lei.

linux.window.cancel = annulla

linux.window.checkingyourjournal = controllo del tuo diario.

linux.window.choosefiles = scegli i file…

linux.window.choosenewdownloadname = scegli un nuovo nome per conservare il file esistente

linux.window.close = chiudi

linux.window.connectajournal = connetti un diario

linux.window.connected = connesso

linux.window.connection = connessione

linux.window.everythinginyourjournal = è tutto nel tuo diario

linux.window.files = file

linux.window.foregroundtitleurlidle = { $route }; primo piano { $foreground }; titolo { $title }; url { $url }; inattivo { $idle }

linux.window.goesalongwithit = va insieme a questo

linux.window.help = aiuto

linux.window.home = ‹ inizio

linux.window.home.vcb176487 = inizio

linux.window.import = importazione

linux.window.keptonthiscomputer = tenuto su questo computer

linux.window.kilobytes = { $size } KB

linux.window.lastreached = ultimo contatto

linux.window.lasttakenin = ultimo ingresso

linux.window.leftout = lasciati fuori

linux.window.megabytes = { $size } MB

linux.window.nothingyet = ancora niente

linux.window.notpaired = non accoppiato

linux.window.obsidian = { $literal1 }

linux.window.offsub = attivalo quando vuoi.

linux.window.ok = ok

linux.window.onthisdevice = su questo dispositivo

linux.window.pause = pausa ▾

linux.window.recent = recenti

linux.window.remove = rimuovi

linux.window.remove.vf022f0fa = rimuovi { $value1 }?

linux.window.resume = riprendi

linux.window.screen = schermo

linux.window.screenaudioandactivitystillon = l'audio e l'attività sono ancora attivi.

linux.window.screenaudiostillon = l'audio è ancora attivo.

linux.window.screenbreath = mentre questo è attivo, l'app di solstone prende ciò che c'è sul tuo schermo, il suono e quali app e finestre stai usando, e tutto va nel tuo diario.

linux.window.screenloginunavailable = non è stato possibile controllare quale sessione è in primo piano.

linux.window.screenmissingelements = l'ingresso dello schermo non può avviarsi: a { $literal1 } manca { $elements }. installa i plugin { $literal2 }, good e base di { $literal1 } con il tuo gestore di pacchetti, poi scegli riprova.

linux.window.screenmissingelementsx11 = l'ingresso dello schermo non può avviarsi: a { $literal1 } manca { $elements }. installa i plugin good e base di { $literal1 } con il tuo gestore di pacchetti, poi scegli riprova.

linux.window.screenretrying = l'ingresso dello schermo non si è avviato al primo tentativo. si riprova tra un momento.

linux.window.screensharingnotallowed = la condivisione dello schermo non è stata consentita. scegli riprova per condividere lo schermo.

linux.window.screensound = il suono comprende il tuo microfono e ciò che questo computer riproduce. se non c'è un microfono, non viene preso nessuno dei due.

linux.window.screenstartfailed = non è stato possibile avviare l'ingresso dello schermo.

linux.window.screenunreachable = non è stato possibile connettersi al tuo desktop.

linux.window.screenwaiting = in attesa di prendere lo schermo.

linux.window.sidebar = barra laterale

linux.window.sofar = finora

linux.window.somethingyouwereaddingdidntfinish = qualcosa che stavi aggiungendo non ha completato il salvataggio. aggiungilo di nuovo.

linux.window.sources = fonti

linux.window.status = stato

linux.window.status.vaff1290e = ‹ stato

linux.window.technicaldetails = dettagli tecnici

linux.window.terminal = terminale

linux.window.terminalbreath = l'app di solstone prende le tue sessioni di { $literal1 }, e tutto va nel tuo diario. richiede { $literal1 } su questo computer, e funziona in qualsiasi sessione del terminale, anche quando entri in questo computer con { $literal2 }.

linux.window.thecopyisalreadygonesothis = la copia è già sparita, quindi questo la cancella solo da qui.

linux.window.thiscomputer = questo computer

linux.window.thisremovesthecopyonthisdevice = questo rimuove la copia su questo dispositivo. il tuo diario può ancora portare la copia che ha già.

linux.window.thisremovesthecopyonthisdevice.v558a0c4f = questo rimuove la copia su questo dispositivo, quindi non arriverà nel tuo diario.

linux.window.trying = tentativo…

linux.window.trynow = prova ora

linux.window.updates = aggiornamenti

linux.window.yourjournal = il tuo diario

linux.window.yourjournal.vc43fd4d5 = tuo · diario
