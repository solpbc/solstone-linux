linux.about.ageday = { $count ->
    [one] hace { $formatted } día
    [many] hace { $formatted } días
   *[other] hace { $formatted } días
}

linux.about.agehour = { $count ->
    [one] hace { $formatted } hora
    [many] hace { $formatted } horas
   *[other] hace { $formatted } horas
}

linux.about.ageminute = { $count ->
    [one] hace { $formatted } minuto
    [many] hace { $formatted } minutos
   *[other] hace { $formatted } minutos
}

linux.about.app = app de escritorio para linux

linux.about.block = { $app }{ $break }{ $journal }

linux.about.journal = journal

linux.about.lastseen = { $journal } · visto por última vez { $age }

linux.about.now = justo ahora

linux.about.unknown = diario desconocido

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

linux.bin.solstoneterminal.couldnotinstallsiginthandler = no se pudo instalar el manejador de { $literal1 }: { $err }

linux.bin.solstoneterminal.couldnotinstallsigtermhandler = no se pudo instalar el manejador de { $literal1 }: { $err }

linux.bin.solstoneterminal.executablemustbeabsolute = { $value1 }: el ejecutable debe ser una ruta absoluta

linux.bin.solstoneterminal.nothinghere = { $value1 }: no hay nada aquí

linux.bin.solstoneterminal.stillrunning = { $value1 }: sigue en ejecución

linux.browser.status.browserconnected = navegador: { $value1 } conectado

linux.browser.status.browserpagesthiscomputerwaiting = { $ws1 }{ $value1 } páginas del navegador en esta computadora, en espera de ir a tu diario

linux.browser.status.chromeedge = Chrome o Edge

linux.browser.status.ifyouhaventyet = { $ws1 }si todavía no lo hiciste, agrega la extensión de solstone a { $served }: { $url }

linux.browser.status.n0kb = { $value1 } KB

linux.browser.status.n1mb = { $value1 } MB

linux.cli.agreewithoutbeingasked = aceptar sin que te pregunten

linux.cli.anothercopyofthesolstoneappisalreadyrunning = Otra copia de la app de solstone ya se está ejecutando para este inicio de sesión, así que esta no se inició. Detén la otra copia y luego vuelve a intentarlo.

linux.cli.browserendpointisnottrusted = el endpoint del navegador no es de confianza

linux.cli.cachex = caché:{ $ws1 }{ $value1 }

linux.cli.cachexnotcreatedyet = caché:{ $ws1 }{ $value1 } (todavía no se creó)

linux.cli.commandoutputexceededitsbound = la salida del comando superó su límite

linux.cli.commandtimedout = se agotó el tiempo de espera del comando

linux.cli.configupdateerrorx = error al actualizar la configuración: { $error }

linux.cli.configx = configuración: { $value1 }

linux.cli.confirmyourjournalsmarkexitcodes0pairedand = confirmar la marca de tu diario.{ $ws1 }Códigos de salida:{ $ws2 }0{ $ws3 }se vinculó y se confirmó, o ya se había confirmado{ $ws4 }1{ $ws5 }sin vincular, nada cambió, o no hay terminal{ $ws6 }2{ $ws7 }error de uso{ $ws8 }5{ $ws9 }retenido

linux.cli.couldnotdiscardthebrowserpagesx = no se pudo descartar las páginas del navegador: { $error }

linux.cli.couldnotstartx = no se pudo iniciar: { $error }

linux.cli.couldntkeepthatlinkforthe = no se pudo guardar ese enlace para la ventana: { $error }

linux.cli.credentialabsent = credencial ausente

linux.cli.discardfailed = no se pudo descartar

linux.cli.discardreplytoolarge = respuesta de descarte demasiado grande

linux.cli.discardwaitingbrowserpages = descartar las páginas del navegador en espera

linux.cli.drawcursor = Dibujar el cursor

linux.cli.editsettings = editar la configuración

linux.cli.enterapositiveinteger = Ingresa un número entero positivo.

linux.cli.enteryorn = Ingresa { $literal1 } o { $literal2 }.

linux.cli.erroreditingsettingsx = Error al editar la configuración: { $error }

linux.cli.errorx = Error: { $error }

linux.cli.howmanyminutestopausefordefaultuntilyouresu = ¿Cuántos minutos pausar (predeterminado: hasta que reanudes)?

linux.cli.installgnomeandkwinactivityhelperassets = instalar los recursos auxiliares de actividad de { $command1 } y { $command2 }

linux.cli.installsystemduserservice = Instalar el servicio de usuario de systemd

linux.cli.invaliddiscardreply = respuesta de descarte no válida

linux.cli.journalversionx = versión del diario: { $version-str }

linux.cli.linkedprivatestateisalreadyinuse = El estado privado enlazado ya está en uso

linux.cli.missingexecutabledirectory = falta el directorio del ejecutable

linux.cli.moveexistingsolstonetmuxinto = mover un solstone para { $command0 } que ya existe a esta app

linux.cli.nosessionbussothereisnodesktoptosetupx = no hay bus de sesión, así que no hay un escritorio que configurar: { $error }

linux.cli.notset = (sin establecer)

linux.cli.pairthisdevicewithyourjournal = vincular este dispositivo con tu diario

linux.cli.pairthisdevicewithyourjournalexitcodes0pair = vincular este dispositivo con tu diario.{ $ws1 }Códigos de salida:{ $ws2 }0{ $ws3 }se vinculó y se confirmó, o ya se había confirmado{ $ws4 }1{ $ws5 }sin vincular, nada cambió, o no hay terminal{ $ws6 }2{ $ws7 }error de uso{ $ws8 }5{ $ws9 }retenido

linux.cli.pauseintake = pausar la recepción

linux.cli.resumeintake = reanudar la recepción

linux.cli.screenaudiodesktopactivityoff = pantalla, audio y actividad del escritorio: activado o desactivado

linux.cli.segmentdurationinsecondsdefault300 = Duración del segmento en segundos (predeterminado: 300)

linux.cli.segmentintervalseconds = Intervalo del segmento en segundos

linux.cli.servicex = { $ws1 }servicio: { $value1 }

linux.cli.settingsboolprompt = { $label } [{ $answer }]:{ $ws1 }

linux.cli.settingsintprompt = { $label } [{ $current }]:{ $ws1 }

linux.cli.settingssavedtox = { $ws1 }Configuración guardada en { $value1 }

linux.cli.setupappalreadyrunning = no se pudo iniciar la configuración inicial porque la app de solstone se está ejecutando. detén la app de solstone primero y vuelve a intentarlo. no se consumió ninguna entrada; el estado de la app, la configuración y el estado privado no cambiaron.

linux.cli.setupconfigfailed = la configuración inicial falló antes de la vinculación porque la app de solstone no pudo actualizar su configuración de forma segura.

linux.cli.setupconnectionfailed = la configuración inicial falló: la app de solstone no pudo conectarse con tu diario.

linux.cli.setupfailedx = la configuración inicial falló: { $error }

linux.cli.setupthegnomepaneliconwherepauseandresumel = configurar el icono del panel de { $command1 }, donde se pausa y se reanuda

linux.cli.showstatus = mostrar el estado

linux.cli.showwhetherterminaliswhat = mostrar si la terminal está activada y qué está haciendo

linux.cli.showyourappandjournalversions = mostrar las versiones de tu app y de tu diario

linux.cli.startpaused = Iniciar en pausa

linux.cli.startupcouldnotcontinuebecausethesolstoneapp = No se pudo continuar el inicio porque la app de solstone no pudo limpiar el estado de sincronización de la ejecución anterior. Asegúrate de que la app de solstone pueda escribir sus datos locales y luego vuelve a intentarlo.

linux.cli.startupcouldnotcontinuebecausethesolstoneapp2 = No se pudo continuar el inicio porque la app de solstone no pudo preparar sus datos locales de forma segura. Asegúrate de que la app de solstone pueda escribir sus datos locales y luego vuelve a intentarlo.

linux.cli.streamnamedefaultstohostnamederived = Nombre del flujo (predeterminado: derivado del nombre del host)

linux.cli.streamx = flujo: { $stream }

linux.cli.terminaloffstatus = terminal: { $command3 } o estado

linux.cli.thesetakeeffectthenexttimethesolstoneappsta = Estos se aplican la próxima vez que se inicie la app de solstone.

linux.cli.thesolstoneappforlinuxtakesinwhatyousharew = la app de solstone para linux recibe lo que compartes con ella, y todo va a tu diario. parte de solstone.

linux.cli.turnterminal = activar la terminal

linux.cli.turnterminaloff = desactivar la terminal

linux.cli.uninstallsystemduserservice = Desinstalar el servicio de usuario de systemd

linux.cli.verifyinstallprerequisites = Verificar los requisitos previos de la instalación

linux.cli.windowscopeexitedx = el ámbito de la ventana terminó: { $status }

linux.cli.xincompletesegments = { $ws1 }{ $value1 } segmento(s) incompleto(s)

linux.cli.xsegmentsacrossxdaysxmb = { $ws1 }{ $value1 } segmentos en { $value2 } día(s), { $value3 } MB

linux.clipboard.agentinstructions = app de solstone para linux (repositorio: { $repo })
    Fuente: { $repository }
    Lee INSTALL.md en { $install } para la configuración y la arquitectura.
    Configuración: { $config_path }
    Segmentos: { $captures_dir }
    Registros: { $logs }
    Servicio: { $service }

linux.control.screenandaudioneedyourdesktopsession = pantalla y audio: necesitan tu sesión de escritorio, y no hay ninguna en ejecución

linux.control.solstonefortmuxhadbeenstoppedso = { $ws1 }solstone para { $literal2 } se había detenido, así que la terminal sigue desactivada

linux.control.terminalhistorycontinued = { $ws1 }historial de la terminal: continuado

linux.control.terminalhistorycontinuedundertheoldpairing = { $ws1 }historial de la terminal: continuado, bajo la vinculación anterior

linux.control.thesolstoneappdidnotanswerin = la app de solstone no respondió a tiempo; vuelve a intentarlo en un momento.

linux.control.thesolstoneappisntrunning = { $ws1 }la app de solstone no se está ejecutando

linux.control.thesolstoneappisntrunningstart = la app de solstone no se está ejecutando; iníciala y vuelve a intentarlo.

linux.control.thesolstoneappisstartingtryagain = la app de solstone se está iniciando; vuelve a intentarlo en un momento

linux.control.thisneedsattentionpleasecontactsupport = { $ws1 }esto necesita atención: por favor, contacta a soporte

linux.copy.aboutheading = acerca de solstone

linux.copy.aboutmadeby = hecho por { $literal1 }.

linux.copy.activityonly = solo actividad

linux.copy.activityroute = ruta de actividad

linux.copy.anythingwaitingsyncstaysthis = todo lo que está en espera de sincronizarse se queda en esta computadora. para ver qué hacer, ejecuta: { $literal1 }

linux.copy.anythingwaitingtosyncstaysonthiscomputerthe = todo lo que está en espera de sincronizarse se queda en esta computadora. la app de solstone volverá a intentarlo cuando se pueda conectar con tu diario.

linux.copy.appsleftoutofdesktopactivity = aplicaciones excluidas de la actividad del escritorio

linux.copy.appversion = versión de la app

linux.copy.attentioncount = { $count ->
    [one] { $formatted } necesita atención
    [many] { $formatted } necesitan atención
   *[other] { $formatted } necesitan atención
}

linux.copy.audio = audio

linux.copy.audiointakecancontinue = la recepción de audio puede continuar.

linux.copy.availablelogsareshownbelowcheckthenotesforg = los registros disponibles se muestran abajo. revisa las notas en busca de huecos antes de guardar una copia.

linux.copy.cancelpairing = cancelar la vinculación

linux.copy.checkingstatus = revisando el estado…

linux.copy.checkstatus = revisar el estado

linux.copy.choose = elegir

linux.copy.choosefolder = elegir una carpeta

linux.copy.choosefolder.v4cba918b = elegir una carpeta…

linux.copy.chooseobsidianvault = elegir tu bóveda de { $literal1 }

linux.copy.configfile = archivo de configuración

linux.copy.confirming = confirmando…

linux.copy.confirmyourjournalsmark = confirmar la marca de tu diario

linux.copy.connected = conectado a

linux.copy.connecting = conectando…

linux.copy.connectjournal = conectar con tu diario

linux.copy.copysaved = copia guardada.

linux.copy.couldntcheckwhetherallyoursettingsweresaved = no se pudo revisar si se guardó toda tu configuración. vuelve a leerla antes de volver a intentarlo.

linux.copy.couldntcheckwhetherintakepausedcheckstatusbe = no se pudo revisar si la recepción se pausó. revisa el estado antes de volver a intentarlo.

linux.copy.couldntcheckwhetherintakeresumedcheckstatusb = no se pudo revisar si la recepción se reanudó. revisa el estado antes de volver a intentarlo.

linux.copy.couldntcheckwhetherpairingfinishedcheckstatus = no se pudo revisar si la vinculación terminó. revisa el estado antes de volver a intentarlo.

linux.copy.couldntopenthatlinkvisitsupportsolstoneappo = no se pudo abrir ese enlace. visita { $literal1 } o escribe a { $literal2 }.

linux.copy.couldntopenthesolstonewindow = no se pudo abrir la ventana de solstone

linux.copy.couldntopenthesolstonewindowopenxfordetails = no se pudo abrir la ventana de solstone. abre { $path } para ver los detalles.

linux.copy.couldntopenyourbrowsertryagainoropenyourjo = no se pudo abrir tu navegador. vuelve a intentarlo, o abre tu diario desde su dirección habitual.

linux.copy.couldntpauseintaketryagain = no se pudo pausar la recepción. vuelve a intentarlo.

linux.copy.couldntreadthelogfilestryagain = no se pudieron leer los archivos de registro. vuelve a intentarlo.

linux.copy.couldntreadthelogstryagain = no se pudieron leer los registros. vuelve a intentarlo.

linux.copy.couldntreadthesolstoneappsstatuscheckwhethe = no se pudo leer el estado de la app de solstone. revisa si se está ejecutando y luego vuelve a intentarlo.

linux.copy.couldntreadyoursettingstryagainbeforemaking = no se pudo leer tu configuración. vuelve a intentarlo antes de hacer cambios.

linux.copy.couldntrestartthesolstoneappcheckthedetails = no se pudo reiniciar la app de solstone. revisa los detalles y luego vuelve a intentarlo.

linux.copy.couldntresumeintaketryagain = no se pudo reanudar la recepción. vuelve a intentarlo.

linux.copy.couldntsaveactivityintaketryagain = no se pudo guardar la recepción de actividad. vuelve a intentarlo.

linux.copy.couldntsaveallyoursettingsyoureditsarestill = no se pudo guardar toda tu configuración. tus cambios siguen aquí. vuelve a intentarlo.

linux.copy.couldntsavethecopytrysavingtoanotherfolder = no se pudo guardar la copia. intenta guardarla en otra carpeta.

linux.copy.couldntsaveyouranswertryagain = no se pudo guardar tu respuesta. vuelve a intentarlo.

linux.copy.couldntsaveyoursettingscheckthedetailsthent = no se pudo guardar tu configuración. revisa los detalles y luego vuelve a intentarlo.

linux.copy.couldntstartthesolstoneappcheckthedetailsth = no se pudo iniciar la app de solstone. revisa los detalles y luego vuelve a intentarlo.

linux.copy.couldntverify = no se pudo verificar

linux.copy.couldntverifyjournalso = no se pudo verificar tu diario, así que no se conectó nada. vuelve a intentarlo, o pega un enlace de vinculación.

linux.copy.couldntworkoutthismarksotheresnothingtoco = no se pudo determinar esta marca, así que no hay nada que comparar. tu vinculación actual se queda en uso.

linux.copy.couldntworkoutyourjournalsmarksotheresnot = no se pudo determinar la marca de ese diario, así que no hay nada que comparar. obtén un nuevo enlace de vinculación de tu diario y luego vuelve a intentarlo.

linux.copy.countwaitingsync = { $count } en espera de sincronizarse

linux.copy.d1 = mientras esto está activado, la app de solstone recibe las capturas de pantalla que tomas en esta computadora, y tu diario guarda cada una en el minuto en que la tomaste.

linux.copy.d10 = algunas herramientas de capturas de pantalla solo guardan cuando se lo pides, y una captura de pantalla que no se guardó no entra.

linux.copy.d15 = más sobre esto

linux.copy.d8 = no se pudo encontrar una carpeta Screenshots en esta computadora. elige la carpeta donde se guardan tus capturas de pantalla.

linux.copy.d8pickertitle = elegir la carpeta donde se guardan tus capturas de pantalla

linux.copy.desktopactivity = actividad de escritorio

linux.copy.desktopactivityisonwheneverthesolstoneapptak = la actividad del escritorio está activada siempre que la app de solstone recibe tu pantalla, y al pausar se pausan ambas. la app de solstone recibe la aplicación, el título de la ventana, las direcciones web y el tiempo activo o inactivo que compartes con ella, y todo va a tu diario.

linux.copy.doesthismatchyourjournal = ¿esto coincide con tu diario?

linux.copy.drawcursor = dibujar el cursor

linux.copy.enterawholenumbergreaterthan0 = ingresa un número entero mayor que 0.

linux.copy.foundjournalthiscomputer = se encontró tu diario en esta computadora

linux.copy.from = de { $folder }

linux.copy.getafreshlink = obtener un enlace nuevo

linux.copy.gethelp = obtener ayuda

linux.copy.healthdetails = detalles del estado

linux.copy.help = ayuda

linux.copy.helpreport = informar un problema

linux.copy.intake = recepción

linux.copy.intakeisntrunning = la recepción no se está ejecutando.

linux.copy.intakeispauseduntilyouresume = la recepción está en pausa hasta que reanudes.

linux.copy.intakeisrunning = la recepción se está ejecutando.

linux.copy.journal = journal

linux.copy.journal3 = tu diario

linux.copy.journaldetails = detalles del diario

linux.copy.journallinkinvalidbody = copia el enlace completo de tu chat y ábrelo de nuevo.

linux.copy.journallinkinvalidtitle = este enlace del diario no es válido.

linux.copy.journalneedsupdatebeforeit = tu diario necesita una actualización antes de poder recibir estas notas. están en espera en esta computadora.

linux.copy.journalsetupaction = configurar un diario en esta computadora

linux.copy.journalsetupfailed = no se pudo preparar tu diario. ejecuta { $literal1 } en una terminal para ver qué necesita atención.

linux.copy.journalsetupinbrowser = termina la configuración inicial en tu navegador.

linux.copy.journalsetupinstall = instalar el diario

linux.copy.journalsetupnotinstalled = el diario todavía no está instalado en esta computadora.

linux.copy.journalsetupopenfailed = tu diario está preparado, pero el navegador no se abrió. ejecuta { $literal1 } en una terminal para terminar la configuración inicial.

linux.copy.journalsetuppreparing = preparando tu diario…

linux.copy.keepdesktopactivityonwhilepaused = mantener la actividad del escritorio activada durante la pausa

linux.copy.lastchecked = última revisión

linux.copy.lastdelivery = última entrega

linux.copy.lastdeliveryvalue = última entrega: { $value }

linux.copy.linuxdoesnotclassifyprivatewindows = linux no clasifica las ventanas privadas.

linux.copy.mark = marca

linux.copy.markunavailable = marca no disponible

linux.copy.moveit = moverlo

linux.copy.movesolstonetmux = ¿mover solstone para { $literal1 } a esta app?

linux.copy.needahandreachahumanwerehappytohelp = ¿necesitas una mano? contáctanos. nos alegra ayudar.

linux.copy.needattention = necesitan atención

linux.copy.needsattention = necesita atención

linux.copy.newpairingcancelled = nueva vinculación cancelada

linux.copy.newpairingcancelledyourcurrentpairingstaysin = nueva vinculación cancelada. tu vinculación actual se queda en uso.

linux.copy.nologfilesareavailableyettryagainafterusing = todavía no hay archivos de registro disponibles. vuelve a intentarlo después de usar la app.

linux.copy.norecentlogsareavailableyettryagainafterusi = todavía no hay registros recientes disponibles. vuelve a intentarlo después de usar la app.

linux.copy.not = no

linux.copy.nothingthiscomputerhaskept = nada de lo que esta computadora ha guardado llegó a ese diario por este enlace. puede que hayas pegado el enlace equivocado, o que algo no esté bien. obtén un nuevo enlace de vinculación de tu diario y vuelve a intentarlo, o escribe a { $literal1 } y te ayudamos.

linux.copy.nothingwaitinggoesintoyourjournaluntilyoudo = nada en espera entra a tu diario hasta que lo hagas.

linux.copy.notpaired = sin vincular

linux.copy.notreachingyourjournalrightnow = ahora no se conecta con tu diario

linux.copy.notthisdevice = no está en este dispositivo

linux.copy.notyet = todavía no

linux.copy.obsidian = { $literal1 }

linux.copy.obsidianwarning = tu diario ya tiene una bóveda llamada { $vault } de otro dispositivo. si continúas, los cambios de ambos dispositivos van a tu diario.

linux.copy.obsidianwarningdate = tu diario ya tiene una bóveda llamada { $vault } de otro dispositivo, actualizada por última vez el { $date }. si continúas, los cambios de ambos dispositivos van a tu diario.

linux.copy.obsidianwarningdevice = tu diario ya tiene una bóveda llamada { $vault } de { $device }. si continúas, los cambios de ambos dispositivos van a tu diario.

linux.copy.obsidianwarningdevicedate = tu diario ya tiene una bóveda llamada { $vault } de { $device }, actualizada por última vez el { $date }. si continúas, los cambios de ambos dispositivos van a tu diario.

linux.copy.off = desactivado

linux.copy.offmeansnonethreegoes = desactivado significa que ninguno de los tres va a tu diario, haga lo que haga tu escritorio.

linux.copy.ofinyourjournal = { $done } de { $total } en tu diario

linux.copy.ofscreenshotsinyourjournal = { $done } de { $total } capturas de pantalla en tu diario

linux.copy.omittedbytes = { $count ->
    [one] { $break }[omitido { $formatted } byte]{ $break }
    [many] { $break }[omitidos { $formatted } bytes]{ $break }
   *[other] { $break }[omitidos { $formatted } bytes]{ $break }
}

linux.copy.on = activado

linux.copy.openthenetworkappinyourjournalandgetapairi = abre la app de red en tu diario y obtén un enlace de vinculación.

linux.copy.openyourjournal = abrir tu diario

linux.copy.otherfilesinstaywheretheyare = los demás archivos en { $folder } se quedan donde están.

linux.copy.pairadevice = vincular un dispositivo

linux.copy.pairagain = vincular de nuevo

linux.copy.pairing = vinculando…

linux.copy.pairingcancelledgetafreshpairinglinkfromyour = vinculación cancelada. obtén un nuevo enlace de vinculación de tu diario cuando quieras.

linux.copy.pairingdidntgothroughgetanewpairinglinkfro = la vinculación no se completó. obtén un nuevo enlace de vinculación de tu diario y vuelve a intentarlo.

linux.copy.pairinglink = enlace de vinculación

linux.copy.partthisvaultcant = no se puede leer parte de esta bóveda. revisa los permisos de las carpetas que tiene dentro. el resto de la bóveda sigue yendo a tu diario.

linux.copy.pasteapairinglinkfromyourjournal = pega un enlace de vinculación de tu diario.

linux.copy.pastepairinglink = pegar enlace de vinculación

linux.copy.pause = pausar

linux.copy.paused = en pausa

linux.copy.pausing = pausando…

linux.copy.presented = presentada

linux.copy.quit = salir de solstone

linux.copy.quitfailed = no se pudo detener la app de solstone. vuelve a intentarlo.

linux.copy.readinglogs = leyendo registros…

linux.copy.readrecentlogs = leer registros recientes

linux.copy.readsettingsagain = volver a leer la configuración

linux.copy.readthelogsbelowthenchoosewhethertosaveaco = lee los registros de abajo y luego elige si guardar una copia.

linux.copy.readthesolstoneappsrecentlogsbeforesavinga = lee los registros recientes de la app de solstone antes de guardar una copia para un reporte de soporte. no se envía nada desde aquí.

linux.copy.recentlogs = registros recientes

linux.copy.restarting = reiniciando…

linux.copy.restartneeded = reinicio necesario

linux.copy.restartthesolstoneapp = reiniciar la app de solstone

linux.copy.resume = reanudar

linux.copy.resuming = reanudando…

linux.copy.s1 = capturas de pantalla

linux.copy.s10counting = revisando…

linux.copy.s10today = empezar desde hoy

linux.copy.s11 = elige qué tan atrás empezar y luego tráelas.

linux.copy.s12 = no se pudieron contar tus capturas de pantalla.

linux.copy.s14 = una captura de pantalla que ya está en tu diario se queda ahí si la eliminas de la carpeta.

linux.copy.s15 = lo que ya está en tu diario se queda cuando pausas o desactivas esto.

linux.copy.s16computer = las capturas de pantalla que tomas mientras está en pausa no entran.

linux.copy.s17 = las capturas de pantalla que ya están en tu diario no se vuelven a traer.

linux.copy.s19 = en espera de ir a tu diario

linux.copy.s21 = en espera de una actualización del diario. tus capturas de pantalla esperan en esta computadora.

linux.copy.s22 = carpeta

linux.copy.s23 = solo entra lo que la app de solstone puede reconocer como captura de pantalla.

linux.copy.s7 = de antes de hoy

linux.copy.s9 = verás cuántas capturas de pantalla hay antes de que entre alguna.

linux.copy.saveacopy = guardar una copia…

linux.copy.savesettings = guardar la configuración

linux.copy.saving = guardando…

linux.copy.savingcopy = guardando la copia…

linux.copy.savingwritesexactlythepreviewabove = al guardar se escribe exactamente la vista previa de arriba.

linux.copy.screen = pantalla

linux.copy.screenandaudioarepauseduntilyouresumedesktop = la pantalla y el audio están en pausa hasta que reanudes. la actividad del escritorio sigue.

linux.copy.screenandaudiostillpausedesktopactivitykeeps = la pantalla y el audio siguen en pausa. la actividad del escritorio sigue.

linux.copy.screenaudiodesktopactivity = pantalla, audio y actividad del escritorio

linux.copy.screenintakeiswaiting = la recepción de pantalla está en espera

linux.copy.screenshotslookback76 = los últimos 30 días

linux.copy.screenshotslookback77 = los últimos 6 meses

linux.copy.screenshotslookback78 = el último año

linux.copy.screenshotslookback79 = todo

linux.copy.screenshotslookback80 = nada de antes de hoy

linux.copy.seconds = segundos

linux.copy.segmentinterval = intervalo del segmento

linux.copy.sentfoundnotesjournal = { $sent } de { $found } notas en tu diario

linux.copy.settings = configuración

linux.copy.settingsactivityonlyhelp = solo actividad del escritorio, con la pantalla y el sonido desactivados.

linux.copy.settingssaved = configuración guardada.

linux.copy.solstoneappcantread = la app de solstone no puede leer esta bóveda. revisa que su carpeta esté ahí y que la unidad donde está esté conectada. nada cambia en tu diario mientras está fuera de alcance.

linux.copy.solstoneappcouldntsave = no se pudo guardar la posición de la app de solstone para esta bóveda. revisa que esta computadora tenga espacio libre.

linux.copy.solstoneappdidntfind = la app de solstone no encontró una bóveda en esta computadora. elige su carpeta en su lugar.

linux.copy.solstoneapptakesnotesone = la app de solstone recibe las notas de una bóveda en esta computadora. todas van a tu diario, y luego cada una cuando cambia. tu bóveda se queda como está.

linux.copy.somenotescouldntgo = algunas notas no pudieron ir a tu diario. la app de solstone las intenta de nuevo la próxima vez que se inicia.

linux.copy.somethingansweredinsteadjournal = algo respondió en lugar de tu diario

linux.copy.somethingansweredthiscomputerat = algo respondió a esta computadora en { $address }, y no es el diario con el que esta computadora está vinculada. podría ser otro dispositivo que ahora usa esa dirección, o tu diario, si su identidad se restableció. si la marca bajo "qué respondió" coincide con la que tu diario muestra ahora, vuelve a vincular esta computadora.

linux.copy.somethingansweredthiscomputerthrough = algo respondió a esta computadora a través del servidor de retransmisión, y no es el diario con el que esta computadora está vinculada. esto tiene menos certeza que una conexión directa.

linux.copy.starting = iniciando…

linux.copy.startpaused = iniciar en pausa

linux.copy.startthesolstoneapp = iniciar la app de solstone

linux.copy.statusunavailable = estado no disponible

linux.copy.syncingjournal = sincronizando con tu diario…

linux.copy.takestmuxsessionsallit = recibe tus sesiones de { $literal1 }, y todo va a tu diario. necesita { $literal1 } en esta computadora, y funciona a través de { $literal2 } y sin escritorio.

linux.copy.terminal = terminal

linux.copy.thatdoesntmatch = eso no coincide

linux.copy.thatfolderisntobsidian = esa carpeta no es una bóveda de { $literal1 }, así que la app de solstone no la agregó. elige la carpeta propia de la bóveda.

linux.copy.thatfoldersnamecan = el nombre de esa carpeta no puede ser el nombre de una bóveda en tu diario. cambia el nombre de la carpeta y vuelve a intentarlo.

linux.copy.thatpairinglinkisntonethesolstoneappcanrea = ese enlace de vinculación no es uno que la app de solstone pueda leer. obtén un enlace de vinculación nuevo de tu diario y vuelve a intentarlo.

linux.copy.thefolderisntonthiscomputer = la carpeta { $folder } ya no está en esta computadora.

linux.copy.thelogpreviewwaslimitedmorelogtextmaybeava = { $ws1 }[la vista previa del registro estaba limitada; puede haber más texto del registro]{ $ws1 }

linux.copy.thepairingchangedwhileyouwerecheckingitsmark = la vinculación cambió mientras revisabas su marca. revisa la marca actual antes de confirmar.

linux.copy.thepairingwindowclosedgetanewpairinglinkfro = la ventana de vinculación se cerró. obtén un enlace de vinculación nuevo de tu diario y luego vuelve a intentarlo.

linux.copy.thesetakeeffectthenexttimethesolstoneappsta = esto entra en vigor la próxima vez que se inicie la app de solstone.

linux.copy.thesolstoneappcannowconnecttoyourjournal = la app de solstone ya puede conectarse con tu diario.

linux.copy.thesolstoneapphasntansweredyetcheckforades = la app de solstone todavía no ha respondido. revisa si hay una solicitud para compartir el escritorio y luego revisa el estado.

linux.copy.thesolstoneapprestartedwithyoursavedsettings = la app de solstone se reinició con tu configuración guardada.

linux.copy.thewindowcouldntstartopenxfordetails = no se pudo iniciar la ventana. abre { $path } para los detalles.

linux.copy.thewindowcouldntstarttryopeningitagain = no se pudo iniciar la ventana. intenta abrirla de nuevo.

linux.copy.thewindowneedsgtk46ornewercouldntloadits = la ventana necesita { $literal1 } 4.6 o posterior. no se pudo cargar sus bibliotecas de escritorio. abre { $path } para los detalles.

linux.copy.thisisjournal = este es tu diario

linux.copy.thismovessolstonetmuxinto = esto mueve solstone para { $literal1 } a esta app, y se detiene para siempre. lo que todavía tiene en espera va a tu diario a través de esta app, y lo que no pueda ir se queda en esta computadora. si esta app todavía no está vinculada, se vincula con el diario que usa solstone para { $literal1 }.

linux.copy.thisstartsthesolstoneappnowandatsigninopen = esto inicia la app de solstone ahora y al iniciar sesión. solo abrir esta ventana no inicia la recepción.

linux.copy.toolargebring = demasiado grande para recibir

linux.copy.tryagain = reintentar

linux.copy.turningthisoffstopsbringing = desactivar esto detiene la recepción de cambios. lo que ya está en tu diario se queda ahí, y lo que está en espera se queda en esta computadora hasta que lo vuelvas a activar.

linux.copy.turnoff = desactivar

linux.copy.unavailable = no disponible

linux.copy.updateshow = con las actualizaciones automáticas activadas, la app se actualiza junto con tu diario cuando el instalador de solstone configuró ambos en esta computadora. si no, vuelve a ejecutar el instalador de solstone con --upgrade.

linux.copy.updatesjournalversion = versión del diario

linux.copy.usethisvault = usar esta bóveda

linux.copy.vault = bóveda

linux.copy.vaultsthiscomputer = bóvedas en esta computadora

linux.copy.vaultwiththisnameis = ya hay una bóveda con este nombre en tu diario

linux.copy.versionvalue = versión de la app: { $value }

linux.copy.viewdetails = ver detalles

linux.copy.waitingforthesolstoneapptoanswerifyourdeskt = en espera de que la app de solstone responda. si tu escritorio pregunta qué compartir, elige ahí.

linux.copy.waitingforyoutoconfirmyourjournalsmark = en espera de que confirmes la marca de tu diario

linux.copy.waitingjournalupdate = en espera de una actualización del diario

linux.copy.waitingreachjournal = en espera de conectar con tu diario

linux.copy.waitingtosync = en espera de sincronizar

linux.copy.waitingvalue = en espera de sincronizar: { $value }

linux.copy.webaddressesturnsondesktopaccessibility = direcciones web (activa la accesibilidad del escritorio)

linux.copy.whatanswered = qué respondió

linux.copy.windowtitles = títulos de ventana

linux.copy.windowtitlesleftoutofdesktopactivity = títulos de ventana fuera de la actividad del escritorio

linux.copy.yesthisismyjournal = sí, este es mi diario

linux.copy.your = tu

linux.copy.yourcurrentpairingstaysinuseuntilyouconfirm = tu vinculación actual se queda en uso hasta que confirmes esta marca.

linux.copy.yourjournalnotsetupyet = tu diario, todavía sin configurar

linux.copy.yourjournalsmarkisconfirmedbutthesolstoneap = la marca de tu diario está confirmada, pero no se pudo reiniciar la app de solstone. vuelve a intentarlo.

linux.copy.yourjournalsmarkisconfirmedbutthesolstoneap2 = la marca de tu diario está confirmada, pero la app de solstone necesita atención. revisa los detalles y luego vuelve a intentarlo.

linux.copy.yourjournalsmarkunavailablerightnow = la marca de tu diario, no disponible ahora

linux.copy.yoursettingsaresavedbutthesolstoneappcouldn = tu configuración está guardada, pero no se pudo reiniciar la app de solstone. vuelve a intentarlo.

linux.copy.yoursettingsaresavedrestartthesolstoneappto = tu configuración está guardada. reinicia la app de solstone para usarla ahora.

linux.copy.yoursettingschangedoutsidethiswindowreadthem = tu configuración cambió fuera de esta ventana. vuelve a leerla antes de guardar.

linux.copy.yousaidthismarkdoesntmatchtheoneyourjourna2 = dijiste que esta marca no coincide con la que muestra tu diario. tu vinculación actual se queda en uso. obtén un enlace de vinculación nuevo de tu diario, o envía un correo a { $literal1 } y te ayudamos.

linux.desktop.clipboardcommandexitedunsuccessfully = el comando del portapapeles terminó sin éxito

linux.desktop.couldnotopenyourjournalwaitforthesolstoneap = No se pudo abrir tu diario. Espera a que la app de solstone vuelva a conectarse y luego vuelve a intentarlo.

linux.desktop.couldntcopyrunsolstonelinuxabouttoselectand = no se pudo copiar. ejecuta `{ $literal1 } about` para seleccionar y copiar tus versiones.

linux.desktop.couldntcopywithoutxinstallitorrunsolstonel = no se pudo copiar sin { $value1 }. instálalo, o ejecuta `{ $literal1 } about` para seleccionar y copiar tus versiones.

linux.desktop.observercommandmustberoutedbytherunloopowne = el comando del observador debe ser encaminado por el dueño del bucle de ejecución

linux.doctor.cantreachyourdesktopxlogoutandbackintore = no se puede conectar con tu escritorio ({ $value1 }); cierra sesión y vuelve a entrar para restaurarlo

linux.doctor.couldnotreachthesessionbustocheckthepaneli = no se pudo conectar con el bus de sesión para revisar el icono del panel

linux.doctor.couldnotreadloginsessionsx = no se pudo leer las sesiones de inicio: { $error }

linux.doctor.couldnotstarttheprobex = no se pudo iniciar el sondeo: { $error }

linux.doctor.displaynotsetstartsolstonelinuxfromthex11gr = { $literal1 } sin definir; inicia { $literal2 } desde la sesión gráfica de { $literal3 }

linux.doctor.doctor9checksxfailedxwarnings = { $ws1 }doctor: 9 revisiones, { $failures } con fallo, { $warnings } avisos

linux.doctor.followingsessionxwayland = siguiendo la sesión { $session } (wayland)

linux.doctor.followingsessionxx11onx = siguiendo la sesión { $session } (x11 en { $display })

linux.doctor.gnomeneedsanextensionrunsolstonelinuxpanelic = { $literal1 } necesita una extensión; ejecuta { $literal2 }

linux.doctor.gstreamerelementsxmissinginstallthepipewirea = de { $literal1 } faltan elemento(s) { $value1 }; instala los paquetes de plugins de { $literal2 } y good/base

linux.doctor.gstreamerinitializationfailedxinstallgstreamer = falló la inicialización de { $literal1 }: { $error }; instala { $literal1 } 1.x

linux.doctor.label.desktopsession = sesión de escritorio

linux.doctor.label.fail = fallo

linux.doctor.label.gstreamer = gstreamer

linux.doctor.label.na = n/a

linux.doctor.label.ok = ok

linux.doctor.label.panelicon = icono del panel

linux.doctor.label.portal = xdg-desktop-portal

linux.doctor.label.pulse = pipewire (pulse)

linux.doctor.label.sessiontype = tipo de sesión

linux.doctor.label.synchealth = estado de sincronización

linux.doctor.label.systemduser = systemd --user

linux.doctor.label.warn = aviso

linux.doctor.label.x11capture = captura x11

linux.doctor.nodesktopsessionofyoursonthis = no hay una sesión de escritorio tuya en esta computadora; la pantalla y el audio necesitan una

linux.doctor.nodesktopsessionofyoursonthiscomputernothing = no hay una sesión de escritorio tuya en esta computadora; la pantalla y el audio esperan hasta que inicies sesión aquí (una sesión en la que inicias sesión de forma remota no cuenta)

linux.doctor.notapplicablenognomeshellonthesessionbus = no aplica (no hay { $literal1 } Shell en el bus de sesión)

linux.doctor.notapplicablenox11display = no aplica (sin display de { $literal1 })

linux.doctor.notapplicablewaylandsession = no aplica (sesión wayland)

linux.doctor.notregisterednotneededonx11usingximagesrc = sin registrar — no hace falta en { $literal1 } (se usa { $literal2 })

linux.doctor.okbrowserregistrationchromechromiumedgeandfir = ok{ $ws1 }registro en navegadores{ $ws2 }Chrome, Chromium, Edge y Firefox

linux.doctor.okfirefoxsnapallowedtostartthesolstoneapp = ok{ $ws1 }Firefox (snap){ $ws2 }con permiso para iniciar la app de solstone

linux.doctor.okfirefoxsnapasksyouoncetoletitstarttheso = ok{ $ws1 }Firefox (snap){ $ws2 }te pide una vez que le permitas iniciar la app de solstone

linux.doctor.orgfreedesktopportaldesktopnotregisteredon = { $literal1 } sin registrar en el bus de sesión; instala o inicia { $literal2 }

linux.doctor.orgfreedesktopportaldesktopregisteredonsession = { $literal1 } registrado en el bus de sesión

linux.doctor.pipewiresrcvp8encandwebmmuxavailable = pipewiresrc, vp8enc y webmmux disponibles

linux.doctor.probepanickedinspectservicelogsandretry = el sondeo entró en pánico; revisa los registros del servicio y vuelve a intentarlo

linux.doctor.pulseaudiocompatibleserverreachable = servidor compatible con { $literal1 } accesible

linux.doctor.pulseaudiocompatibleserverunreachablexstartpip = servidor compatible con { $literal1 } no accesible: { $error }; inicia { $literal2 } Pulse o { $literal1 }

linux.doctor.screenaudioanddesktopactivityareoff = la pantalla, el audio y la actividad del escritorio están desactivados

linux.doctor.sessionbusprobefailedxstartagraphicalsession = falló el sondeo del bus de sesión: { $error }; inicia una sesión gráfica y asegúrate de que { $literal1 } esté instalado

linux.doctor.sessionbusunreachablexstartagraphicalsession = bus de sesión no accesible: { $error }; inicia una sesión gráfica y asegúrate de que { $literal1 } esté instalado

linux.doctor.systemctlusernotreachableruninsideasystemdus = { $literal1 } no accesible; ejecuta dentro de una sesión de usuario de systemd

linux.doctor.systemctlusernotreachablexruninsideasystemd = { $literal1 } no accesible: { $error }; ejecuta dentro de una sesión de usuario de systemd

linux.doctor.thegnomeextensionisinstalledbutoffrunsolston = la extensión de { $literal1 } está instalada, pero desactivada; ejecuta { $literal2 }

linux.doctor.thegnomeextensionisonbutnopaneliconyetlog = la extensión de { $literal1 } está activada, pero todavía no hay icono del panel; cierra sesión y vuelve a entrar si sigue así

linux.doctor.thepaneliconextensionisinstalledlogoutandba = la extensión del icono del panel está instalada; cierra sesión y vuelve a entrar para terminar

linux.doctor.thisdesktopcanshowthepanelicon = este escritorio puede mostrar el icono del panel

linux.doctor.timedoutafter2schecktheuserdbussessionand = se agotó el tiempo tras 2s; revisa la sesión D-Bus del usuario y { $literal1 }

linux.doctor.unrecognizedsessiontypexwaylandorx11required = tipo de sesión no reconocido '{ $value }'; se requiere { $literal1 } o { $literal2 }

linux.doctor.warnbrowserregistrationmissingoroutofdatefor = aviso{ $ws1 }registro en navegadores{ $ws2 }falta o está desactualizado para { $value1 }; inicia la app de solstone para volver a escribirlos

linux.doctor.warnchromiumsnapcantreachthesolstoneappasn = aviso{ $ws1 }Chromium (snap){ $ws2 }no se puede conectar con la app de solstone; un snap de Chromium no tiene forma de iniciarla. usa Chrome, Edge o Firefox

linux.doctor.warnfirefoxsnapasksyouoncetoletitstartthe = aviso{ $ws1 }Firefox (snap){ $ws2 }te pide una vez que le permitas iniciar la app de solstone; doctor no pudo leer lo que respondiste

linux.doctor.warnfirefoxsnapyoutoldyourdesktopnottoletf = aviso{ $ws1 }Firefox (snap){ $ws2 }le dijiste a tu escritorio que no permitiera que Firefox iniciara la app de solstone, así que no se recibe nada desde Firefox. para que te vuelva a preguntar, ejecuta:{ $ws3 }gdbus { $value1 } --method { $literal4 } { $PERMISSION-TABLE } { $value2 } { $SNAP-FIREFOX }

linux.doctor.warnxcantreachthesolstoneappfrominsideafl = aviso{ $ws1 }{ $id }{ $ws1 }no se puede conectar con la app de solstone desde dentro de un flatpak; usa Chrome, Edge o Firefox instalados fuera de flatpak

linux.doctor.wayland = wayland

linux.doctor.x11randrandximagesrcavailable = { $literal1 } RandR y { $literal2 } disponibles

linux.doctor.x11randrunavailablexcheckdisplayandxserverr = { $literal1 } RandR no disponible: { $error }; revisa { $literal2 } y el soporte RandR del servidor X

linux.doctor.x11usingximagesrccapture = x11 (usando captura de { $literal1 })

linux.doctor.xcheckthatsystemdlogindisrunning = { $value1 }; revisa que { $literal1 } esté en ejecución

linux.doctor.xdgsessiontypenotsetwaylandor = { $literal1 } sin definir; se requiere { $literal2 } o { $literal3 }

linux.doctor.ximagesrcmissinginstallthegstreamergoodplugins = falta { $literal1 }; instala el paquete de plugins good de { $literal2 }

linux.doctor.yourdesktopsessionisnttheoneinfrontofthes = tu sesión de escritorio no es la que está frente a la pantalla; la pantalla y el audio esperan hasta que lo sea

linux.health.idleconfirmingwithyourjournal = en reposo, confirmando con tu diario

linux.health.idleconnected = en reposo, conectado

linux.health.idleconnecting = en reposo, conectando

linux.health.idleconnectionunavailableheldonthisdevice = en reposo, conexión no disponible (se guarda en este dispositivo)

linux.health.idleconnectionunavailableheldonthisdevice2 = en reposo, conexión no disponible, se guarda en este dispositivo

linux.health.idlenostatus = en reposo, sin estado

linux.health.idleofflineheldonthisdevice = en reposo, sin conexión (se guarda en este dispositivo)

linux.health.idleofflineheldonthisdevice2 = en reposo, sin conexión, se guarda en este dispositivo

linux.health.idlepairagain = en reposo, vincular de nuevo

linux.health.idlepairingnotsaved = en reposo, vinculación no guardada

linux.health.idlepairingrequired = en reposo, vinculación requerida

linux.health.idlepairingunsafe = en reposo, vinculación no segura

linux.health.idlesyncing = en reposo, sincronizando

linux.health.idlesyncuptodate = en reposo, sincronización al día

linux.health.idleupdaterequired = en reposo, actualización requerida

linux.health.journalmark = tu diario:{ $ws1 }{ $paired }

linux.health.markclaim = { $spoken }{ $ws1 }({ $claim })

linux.health.markclaimed = declarado, no verificado

linux.health.n0pending = 0 pendientes

linux.health.noidentitypresented = ninguna identidad presentada

linux.health.onconfirmingwithyourjournal = activado, confirmando con tu diario

linux.health.onconnected = activado, conectado

linux.health.onconnecting = activado, conectando

linux.health.onconnectionunavailableheldonthisdevice = activado, conexión no disponible (se guarda en este dispositivo)

linux.health.onconnectionunavailableheldonthisdevice2 = activado, conexión no disponible, se guarda en este dispositivo

linux.health.onnostatus = activado, sin estado

linux.health.onofflineheldonthisdevice = activado, sin conexión (se guarda en este dispositivo)

linux.health.onofflineheldonthisdevice2 = activado, sin conexión, se guarda en este dispositivo

linux.health.onpairagain = activado, vincular de nuevo

linux.health.onpairingnotsaved = activado, vinculación no guardada

linux.health.onpairingrequired = activado, vinculación requerida

linux.health.onpairingunsafe = activado, vinculación no segura

linux.health.onsyncing = activado, sincronizando

linux.health.onsyncuptodate = activado, sincronización al día

linux.health.onupdaterequired = activado, actualización requerida

linux.health.pendingunconfirmed = pendiente sin confirmar

linux.health.sighting = { $header }:{ $ws1 }qué respondió:{ $ws2 }{ $answer }{ $ws3 }{ $paired }{ $footer }

linux.health.syncconfirmingwithyourjournal = sincronización: confirmando con tu diario

linux.health.syncconnecteduptodate = sincronización: conectado; al día

linux.health.syncconnecting = sincronización: conectando

linux.health.syncconnectingwaitwhilethisdeviceconnectstoy = sincronización: conectando; espera mientras este dispositivo se conecta con tu diario

linux.health.syncconnectionunavailableheldonthisdevice = sincronización: conexión no disponible; se guarda en este dispositivo

linux.health.syncconnectionunavailableheldonthisdevicerest = sincronización: conexión no disponible; se guarda en este dispositivo; reinicia la app de solstone; si esto continúa, vuelve a vincular este dispositivo

linux.health.syncconnectionunavailablerestartthesolstoneapp = sincronización: conexión no disponible; reinicia la app de solstone; si esto continúa, vuelve a vincular este dispositivo

linux.health.synchealthconnectingwaitwhilethisdeviceconnec = estado de sincronización: conectando; espera mientras este dispositivo se conecta con tu diario

linux.health.synchealthconnectionunavailablerestartthesolst = estado de sincronización: conexión no disponible; reinicia la app de solstone; si esto continúa, vuelve a vincular este dispositivo

linux.health.synchealthofflineheldonthisdevicewillretry = estado de sincronización: sin conexión; se guarda en este dispositivo; se reintentará

linux.health.synchealthpairingnotsavedfixthisdevicespair = estado de sincronización: vinculación no guardada; corrige los permisos de vinculación de este dispositivo y luego reinicia la app de solstone

linux.health.synchealthpairingunsaferepairthisdevicespair = estado de sincronización: vinculación no segura; repara la vinculación de este dispositivo y reinicia la app de solstone

linux.health.synchealthpairthisdevicewithyourjournal = estado de sincronización: vincula este dispositivo con tu diario

linux.health.synchealthpairthisdevicewithyourjournalagain = estado de sincronización: vuelve a vincular este dispositivo con tu diario

linux.health.synchealthupdaterequiredupdatethesolstoneapp = estado de sincronización: actualización requerida; actualiza la app de solstone

linux.health.synchealthwaitwhilethisdeviceconfirmswithyou = estado de sincronización: espera mientras este dispositivo confirma con tu diario

linux.health.synchealthyourjournalisreceivingnotconfirmed = estado de sincronización: sincronizando; todavía sin confirmación de tu diario

linux.health.syncingfallback = sincronizando...

linux.health.syncnostatus = sincronización: sin estado

linux.health.syncnostatusfromthesolstoneapprightnow = sincronización: sin estado de la app de solstone en este momento

linux.health.syncnostatusthesolstoneappisnotrunningorha = sincronización: sin estado; la app de solstone no está en ejecución, o todavía no tiene estado

linux.health.syncofflineheldonthisdevicewillretry = sincronización: sin conexión; se guarda en este dispositivo; se reintentará

linux.health.syncofflinewillretry = sincronización: sin conexión; se reintentará

linux.health.syncpairagain = sincronización: vincular de nuevo

linux.health.syncpairagainpairthisdevicewithyourjournala = sincronización: vincular de nuevo; vuelve a vincular este dispositivo con tu diario

linux.health.syncpairingnotsaved = sincronización: vinculación no guardada

linux.health.syncpairingnotsavedfixthisdevicespairingper = sincronización: vinculación no guardada; corrige los permisos de vinculación de este dispositivo y luego reinicia la app de solstone

linux.health.syncpairingrequired = sincronización: vinculación requerida

linux.health.syncpairingrequiredpairthisdevicewithyourjou = sincronización: vinculación requerida; vincula este dispositivo con tu diario

linux.health.syncpairingunsafe = sincronización: vinculación no segura

linux.health.syncpairingunsaferepairthisdevicespairingand = sincronización: vinculación no segura; repara la vinculación de este dispositivo y reinicia la app de solstone

linux.health.syncpairthisdevicewithyourjournal = sincronización: vincula este dispositivo con tu diario

linux.health.syncpairthisdevicewithyourjournalagain = sincronización: vuelve a vincular este dispositivo con tu diario

linux.health.syncprogress = sincronización: { $progress }

linux.health.syncsyncingyourjournalisreceivingnotconfirmed = sincronización: sincronizando; todavía sin confirmación de tu diario

linux.health.syncupdaterequiredupdatethesolstoneapp = sincronización: actualización requerida; actualiza la app de solstone

linux.health.syncupdatethesolstoneapp = sincronización: actualiza la app de solstone

linux.health.syncuptodate = sincronización: al día

linux.health.syncwaitwhilethisdeviceconfirmswithyourjourn = sincronización: espera mientras este dispositivo confirma con tu diario

linux.health.syncwaitwhilethisdeviceconnectstoyourjournal = sincronización: espera mientras este dispositivo se conecta con tu diario

linux.health.thesolstoneappisnotrunningorhasnostatusyet = la app de solstone no está en ejecución, o todavía no tiene estado

linux.health.unknownjournalseenatx = diario desconocido visto en { $address }

linux.health.unknownjournalseenthroughtherelay = diario desconocido visto a través del servidor de retransmisión

linux.health.uptodateat = estado de sincronización: al día a las { $timestamp }

linux.health.waitingforyoutoconfirmyourjournalsmarknothi = en espera de que confirmes la marca de tu diario. nada de lo que está en espera entra a tu diario hasta que lo hagas.{ $ws1 }cuando quieras, ejecuta: { $literal2 }

linux.import.allhistory = todo el historial de importación en tu diario

linux.import.attention = necesita atención

linux.import.cannotconvert = este dispositivo no puede convertir esta foto

linux.import.delivered = en tu diario.

linux.import.expired = venció

linux.import.explanation = agrega cualquier archivo que elijas aquí o sueltes en esta ventana. tu diario averigua qué es.

linux.import.explanationnodrop = agrega cualquier archivo que elijas aquí. tu diario determina qué es.

linux.import.filekind = tu diario no puede recibir este tipo de archivo

linux.import.filemissing = falta la copia en este dispositivo

linux.import.filenotread = no se pudo leer esto. agrégalo de nuevo

linux.import.filetoolarge = es más grande de lo que tu diario recibe

linux.import.held = por ahora no van a tu diario. cada uno dice por qué.

linux.import.inyourjournalrel = en tu diario · { $when }

linux.import.journalrefusal = tu diario no pudo aceptar esto.

linux.import.moreimports = más importaciones

linux.import.moreimportssub = guías, opciones y el historial de importación en tu diario

linux.import.namedrefusal = { $name }: { $reason }

linux.import.nfiles = { $count } archivos

linux.import.noroom = no hay suficiente espacio en este dispositivo

linux.import.nothingwassaved = no se guardó nada en este dispositivo.

linux.import.nothingyet = todavía no hay nada.

linux.import.nreadyinjournal = { $count } listos en tu diario

linux.import.nwaiting = { $count } en espera

linux.import.pairagain = tu diario dejó de aceptar este dispositivo. las importaciones esperan aquí hasta que vuelvas a vincular.

linux.import.readytoreview = listo para revisar

linux.import.readytoreviewcount = { $count } listos para revisar

linux.import.recentrow.a11y = { $name }, en tu diario, { $when }. abrir en tu diario.

linux.import.remove.a11y = quitar { $name }

linux.import.removeerror = no se pudo quitar esto. vuelve a intentarlo.

linux.import.removereadytoreview = esto quita la copia de este dispositivo. la copia en espera en tu diario se queda ahí hasta que la descartes o venza.

linux.import.reviewdetail = se guardó en tu diario. previsualiza y confirma para empezar.

linux.import.reviewdetailhour = se guardó en tu diario. revisa en menos de una hora, antes de que tu diario quite la copia en espera.

linux.import.reviewexpireddetail = la copia en espera en tu diario venció. este dispositivo todavía guarda el archivo.

linux.import.reviewinjournal = revisar en tu diario

linux.import.reviewinjournal.a11y = revisar { $name } en tu diario

linux.import.saved = guardados en este dispositivo: { $count }

linux.import.saving = guardando en este dispositivo: { $saved } de { $total }

linux.import.sendagain = enviar de nuevo para revisar

linux.import.sendagain.a11y = enviar { $name } de nuevo para revisar

linux.import.sending = enviando

linux.import.sendingellipsis = enviando a tu diario…

linux.import.sendingtojournal = enviando a tu diario.

linux.import.statusneedsyou = las importaciones te necesitan

linux.import.statusneedsyousub = abre la importación para ver por qué.

linux.import.statusreadytoreview = { $count ->
    [one] { $display } importación lista para revisar
    [many] { $display } importaciones listas para revisar
   *[other] { $display } importaciones listas para revisar
}

linux.import.statusreadytoreviewsub = abre la importación para revisar y empezar.

linux.import.statusrow.a11y = importación, { $detail }.

linux.import.steperror = no se pudo terminar este paso. vuelve a intentarlo.

linux.import.storeerror = no se pudieron leer las importaciones en este dispositivo. no se quitó nada. reinicia la app de solstone para volver a intentarlo.

linux.import.tile.a11y = importación, { $state }. { $detail }.

linux.import.trynow.a11y = intentar { $name } ahora

linux.import.waitforjournal = las importaciones esperan en este dispositivo hasta que conectes un diario.

linux.import.waitformark = las importaciones esperan hasta que confirmes la marca de tu diario.

linux.import.waitforupdate = las importaciones esperan hasta que tu diario se actualice.

linux.import.waiting = en espera

linux.import.waitingdetail = en espera de ir a tu diario

linux.import.waitingforjournal = en espera de ir a tu diario.

linux.import.waitingforjournalshort = en espera de tu diario

linux.import.waitingforyourreview = en espera de tu revisión

linux.import.waitingreconnect = en espera de que tu diario vuelva a conectarse.

linux.journalmark.journal = diario: { $value1 }·{ $value2 }

linux.journalmark.journaljournal = diario: tu diario

linux.journalmark.journalnotpaired = diario: sin vincular

linux.journalmark.journalsmarkunavailableright = la marca de tu diario: no disponible ahora

linux.journalmark.markwordsyougavedon = las palabras de la marca que diste no coinciden con la marca del diario, así que esta computadora no está vinculada. revisa las palabras, obtén un nuevo enlace de vinculación de tu diario y vincula de nuevo.

linux.journalmark.markwordsyougavedon2 = las palabras de la marca que diste no coinciden con la marca del diario.

linux.journalmark.notchangedcurrentpairingstays = sin cambios. tu vinculación actual se queda en uso. nada de lo que esta computadora ha guardado llegó a ese diario por este enlace.

linux.journalmark.notpairedgetfreshpairing = sin vincular. obtén un nuevo enlace de vinculación de tu diario y vincula esta computadora.

linux.journalmark.pairingcancelledcurrentpairingstays = vinculación cancelada. tu vinculación actual se queda en uso.

linux.journalmark.pairingcancelledgetfreshpairing = vinculación cancelada. obtén un nuevo enlace de vinculación de tu diario y vincula de nuevo cuando quieras.

linux.journalmark.pastepairinglinkfromjournal = pega el enlace de vinculación de tu diario y luego presiona enter:{ $ws1 }

linux.journalmark.setupfailedpairinglinkwas = la configuración inicial falló: el enlace de vinculación no era válido.

linux.journalmark.yousaidthismarkdoesn = dijiste que esta marca no coincide con la que muestra tu diario, así que esta computadora no está vinculada. puede que hayas pegado el enlace equivocado, o que algo no esté bien. obtén un nuevo enlace de vinculación de tu diario y vincula de nuevo, o escribe a { $literal1 } y te ayudamos.

linux.metadata.autostartcomment = la app de solstone recibe lo que compartes con ella, y todo va a tu diario

linux.metadata.autostartname = app de solstone

linux.metadata.gnomedescription = auxiliar de actividad del escritorio para { $command }

linux.metadata.gnomename = auxiliar de actividad de solstone

linux.metadata.kwindescription = Auxiliar de actividad de KWin para { $command }

linux.metadata.kwinname = auxiliar de actividad de solstone

linux.metadata.name = solstone

linux.metadata.packagedescription = la app de solstone para linux. recibe la pantalla y el audio que compartes con ella, y todo va a tu diario.

linux.metadata.packagesummary = la app de solstone para linux: lo que compartes va a tu diario

linux.metadata.service = app de solstone para linux

linux.notification.copied = copiado

linux.notification.screenelementsmissing = la recepción de pantalla no puede iniciarse: a { $literal1 } le faltan { $elements }. instala { $literal1 } { $literal2 } y los complementos good y base con tu gestor de paquetes y luego elige reintentar en la página de pantalla.

linux.notification.screenelementsmissingx11 = la recepción de pantalla no puede iniciarse: a { $literal1 } le faltan { $elements }. instala { $literal1 } y los complementos good y base con tu gestor de paquetes y luego elige reintentar en la página de pantalla.

linux.pair.cancel = cancelar

linux.pair.continue = continuar

linux.pair.onemorestepcheckyourjournalsmarkyourjournal = un paso más: revisa la marca de tu diario.{ $ws1 }{ $ws2 }la marca de tu diario: { $spoken }{ $ws1 }{ $ws1 }tu diario muestra esta misma marca en su app de red. debe coincidir, exactamente.{ $ws1 }¿esto coincide con tu diario? escribe { $literal3 } o { $literal4 }:{ $ws1 }

linux.pair.onemorestepcheckyourjournalsmarkyourjournal2 = un paso más: revisa la marca de tu diario.{ $ws1 }{ $ws2 }la marca de tu diario: no disponible en este momento{ $ws1 }{ $ws1 }no se pudo verificar.{ $ws1 }en esta computadora no se pudo determinar la marca de tu diario, así que no hay nada que comparar. { $literal3 } solo si tienes la certeza de que el enlace vino de tu diario.{ $ws1 }escribe { $literal3 } para vincular de todos modos, o { $literal4 } para detener:{ $ws1 }

linux.pair.pairingcancellednothingthiscomputerhaskeptwen = vinculación cancelada. nada de lo que esta computadora ha guardado llegó a ese diario por este enlace. obtén un nuevo enlace de vinculación de tu diario y vuelve a iniciar la configuración inicial cuando quieras.

linux.pair.themarkwordsyougavedontmatchthejournalsma = las palabras de la marca que diste no coinciden con la marca del diario, así que esta computadora no está vinculada, y nada de lo que ha guardado llegó a ese diario por este enlace. revisa las palabras, obtén un nuevo enlace de vinculación de tu diario y vuelve a iniciar la configuración inicial.

linux.pair.thiscomputercouldntworkoutthejournalsmarks = en esta computadora no se pudo determinar la marca del diario, así que las palabras que diste no se pueden hacer coincidir. esta computadora no está vinculada, y nada de lo que ha guardado llegó a ese diario por este enlace. obtén un nuevo enlace de vinculación de tu diario e inicia la configuración inicial en una terminal para decidir por ti.

linux.pair.thiscomputercouldntworkoutthejournalsmarks2 = en esta computadora no se pudo determinar la marca del diario, así que las palabras que diste no se pueden hacer coincidir. en espera de que confirmes la marca de tu diario. nada de lo que está en espera entra a tu diario hasta que lo hagas. ejecuta { $literal1 } en una terminal para decidir por ti.

linux.pair.whenyourereadyrunsolstonelinuxconfirm = cuando quieras, ejecuta: { $literal1 }

linux.pair.yousaidthismarkdoesntmatchtheoneyourjourna = dijiste que esta marca no coincide con la que muestra tu diario, así que esta computadora no está vinculada, y nada de lo que ha guardado llegó a ese diario por este enlace. puede que hayas pegado el enlace equivocado, o que algo no esté bien. obtén un nuevo enlace de vinculación de tu diario y vuelve a iniciar la configuración inicial, o escribe a { $literal1 } y te ayudamos.

linux.panel.couldnotreachthedesktoptocheckwhatitneeds = no se pudo conectar con el escritorio para revisar lo que necesita

linux.panel.couldnotreachthesolstoneappx = no se pudo conectar con la app de solstone: { $reason }

linux.panel.couldnotreachthisdesktoptofindoutwhatitnee = no se pudo conectar con este escritorio para averiguar lo que necesita.

linux.panel.donethepaneliconisinyourpanelnow = listo. el icono del panel ya está en tu panel.

linux.panel.dontaskagain = no volver a preguntar

linux.panel.erroractivityhelperassetsdirectorynotfound = error: no se encontró el directorio de recursos auxiliares de actividad

linux.panel.errorfailedtocopygnomeactivityhelperx = error: no se pudo copiar el auxiliar de actividad de { $literal1 }: { $err }

linux.panel.errorfailedtocopykwinactivityhelperx = error: no se pudo copiar el auxiliar de actividad de { $literal1 }: { $err }

linux.panel.errornoactivityhelperassetsfoundinx = error: no se encontraron recursos auxiliares de actividad en { $source }

linux.panel.gnomegaveananswerwedonotknowx = { $literal1 } dio una respuesta que no conocemos: { $other }

linux.panel.gnomeinstalledtheextensionbutcouldnotturnit = { $literal1 } instaló la extensión, pero no pudo activarla

linux.panel.gnomeissetnottoloadextensionssothepanelico = { $literal1 } está configurado para no cargar extensiones, así que el icono del panel no puede volver de esta forma

linux.panel.gnomeneedsoneextensionbeforethepaneliconcan = { $literal1 } necesita una extensión para que el icono del panel pueda aparecer. ese icono es donde se pausa y se reanuda.

linux.panel.gnometooktheextensionbutthepaneliconhasnot = { $literal1 } tomó la extensión, pero el icono del panel no ha aparecido. ejecuta { $literal2 } doctor para ver a dónde llegó.

linux.panel.gnometooktheextensionbutthepaneliconhasnot2 = { $literal1 } tomó la extensión, pero el icono del panel todavía no ha aparecido. espera un momento y luego ejecuta: { $literal2 } doctor

linux.panel.gnomewillaskyoutoconfirmacceptittoaddthep = { $literal1 } te pedirá que confirmes. acéptalo para agregar la extensión del icono del panel.

linux.panel.gnomewouldnotturntheextensionon = { $literal1 } se negó a activar la extensión

linux.panel.installedactivityhelperassetstouserdatadirect = se instalaron los recursos auxiliares de actividad en los directorios de datos del usuario.

linux.panel.logoutandbackinthenrunthisagain = cierra sesión y vuelve a entrar, luego vuelve a ejecutar esto.

linux.panel.nochangeyoucanrunthisagainanytime = sin cambios. puedes volver a ejecutar esto en cualquier momento.

linux.panel.noinstalledappindicatorextensiontoturnon = no hay una extensión appindicator instalada para activar

linux.panel.noteyoumustexplicitlyenabletheextensionorscr = nota: debes activar explícitamente la extensión o el script en tu sesión de escritorio.

linux.panel.nothingherecansetupapanelicononthisdesktop = aquí nada puede configurar un icono del panel en este escritorio.

linux.panel.nothingtodo = nada que hacer.

linux.panel.nothingtosetuphere = nada que configurar aquí.

linux.panel.notnow = ahora no

linux.panel.notrunning = la app de solstone no está en ejecución, así que no hay nada que pausar o reanudar.

linux.panel.pausedforxminutesrunsolstonelinuxresumetoe = en pausa por { $minutes } minuto(s). ejecuta { $literal1 } resume para terminarla antes.

linux.panel.pausedrunsolstonelinuxresumewhenyouwantitba = en pausa. ejecuta { $literal1 } resume cuando quieras que vuelva.

linux.panel.setitup = configurarlo

linux.panel.solstoneapp = app de solstone

linux.panel.thedownloadedextensioncouldnotbeunpacked = no se pudo desempaquetar la extensión descargada

linux.panel.theextensioncouldnotbedownloadedcheckthenetw = no se pudo descargar la extensión; revisa la conexión de red

linux.panel.thegnomeextensionisalreadyonifthepanelicon = la extensión de { $literal1 } ya está activada. si todavía falta el icono del panel, cierra sesión y vuelve a entrar.

linux.panel.thegnomeextensionisinstalledbutturnedoffturn = la extensión de { $literal1 } está instalada, pero desactivada. activándola ahora.

linux.panel.thepaneliconextensionisinstalledbutgnomehas = la extensión del icono del panel está instalada, pero { $literal1 } todavía no la ha reconocido.

linux.panel.thepaneliconextensionisinstalledlogoutandba = la extensión del icono del panel está instalada. cierra sesión y vuelve a entrar, luego vuelve a ejecutar { $literal1 }.

linux.panel.thepaneliconisalreadyavailableonthisdesktop = el icono del panel ya está disponible en este escritorio.

linux.panel.thepaneliconisnotsetupx = el icono del panel no está configurado: { $reason }

linux.panel.thepaneliconisreadypauseandresumeareinthe = el icono del panel está listo. pausar y reanudar ya están en el panel.

linux.panel.thesolstoneappisnotrunningsothereisnothing = la app de solstone no está en ejecución, así que no hay nada que pausar o reanudar.

linux.panel.thisdesktopdoesnotallowextensionstobeinstall = este escritorio no permite instalar extensiones

linux.panelicon.resumed = reanudado.

linux.screenshots.bringcount = { $count ->
    [one] recibir 1 captura de pantalla
    [many] recibir { $formatted } capturas de pantalla
   *[other] recibir { $formatted } capturas de pantalla
}

linux.screenshots.refusedcount = { $count ->
    [one] 1 captura de pantalla no se puede recibir, así que se queda en la carpeta.
    [many] { $formatted } capturas de pantalla no se pueden recibir, así que se quedan en la carpeta.
   *[other] { $formatted } capturas de pantalla no se pueden recibir, así que se quedan en la carpeta.
}

linux.screenshots.total = { $count ->
    [one] 1 captura de pantalla
    [many] { $formatted } capturas de pantalla
   *[other] { $formatted } capturas de pantalla
}

linux.screenshots.unreadablecount = { $count ->
    [one] no se pudo leer 1 captura de pantalla.
    [many] no se pudieron leer { $formatted } capturas de pantalla.
   *[other] no se pudieron leer { $formatted } capturas de pantalla.
}

linux.service.currentexecutablehasnoparentdirectory = el ejecutable actual no tiene directorio padre

linux.service.errorremovingxx = Error al quitar { $value1 }: { $error }

linux.service.errorwritingservicefilesx = Error al escribir los archivos del servicio: { $error }

linux.service.homeisnotsetcannotlocateuserservicefiles = HOME no está definida; no se pueden ubicar los archivos de servicio del usuario

linux.service.invalidservicepid = el PID de servicio no es válido

linux.service.removed = Se quitó { $value1 }

linux.service.servicepidunavailable = el PID de servicio no está disponible

linux.service.servicerestartfailedx = Falló el reinicio del servicio: { $error }

linux.service.systemctlcouldnotrestartsolstonelinuxservicex = systemctl no pudo reiniciar { $literal1 }.service:{ $ws2 }{ $value1 }

linux.service.systemctlnotfound = no se encontró systemctl

linux.service.systemctlstopfailed = systemctl stop falló

linux.service.warningcouldnotremoveerror = Advertencia: no se pudo quitar { $value1 }: { $error }

linux.service.warningcouldnotremoveeverybrowserregistration = Advertencia: no se pudieron quitar todos los registros en navegadores: { $error }

linux.service.warningcouldnotwriteeverybrowserregistrationx = Advertencia: no se pudieron escribir todos los registros en navegadores: { $error }

linux.service.warningsystemctlenablenowfailedrunsystemctlus = Advertencia: systemctl enable --now falló; ejecuta '{ $literal1 }' para inspeccionar el error

linux.service.warningsystemctlenablenowfailedxrunsystemctl = Advertencia: systemctl enable --now falló: { $error }; ejecuta '{ $literal1 }' después de que systemd esté disponible

linux.service.warningsystemctlstartfailedrunsystemctluserst = Advertencia: systemctl start falló; ejecuta '{ $literal1 }' para inspeccionar el error

linux.service.warningsystemctlstartfailedxrunsystemctluser = Advertencia: systemctl start falló: { $error }; ejecuta '{ $literal1 }' después de que systemd esté disponible

linux.service.warningsystemctlxfailedrunsystemctluserxtoi = Advertencia: systemctl { $operation } falló; ejecuta '{ $literal1 } { $operation }' para inspeccionar el error

linux.service.warningsystemctlxfailedxrunsystemctluserxaf = Advertencia: systemctl { $operation } falló: { $error }; ejecuta '{ $literal1 } { $operation }' después de que systemd esté disponible

linux.service.wrotex = Se escribió { $value1 }

linux.shared.cli.versionlastknown = { $value1 } (última conocida)

linux.shared.copy.connected = conectado

linux.shared.copy.status = estado

linux.shared.copy.yourjournalshowsthissamemarkinitsnetworkapp = tu diario muestra esta misma marca en su app de red. debe coincidir, exactamente.

linux.shared.health.unknown = desconocido

linux.shared.pair.confirmcantaskyouaboutyourjournalsmarkhere = confirm no puede preguntarte por la marca de tu diario aquí. ejecútalo en una terminal, o da las dos palabras de la marca con --mark "word word".

linux.shared.pair.couldntverify = no se pudo verificar.

linux.shared.pair.doesthismatchyourjournaltypeyesorno = ¿esto coincide con tu diario? escribe { $literal1 } o { $literal2 }:{ $ws3 }

linux.shared.pair.givethetwowordsofyourjournalsmarklikemark = da las dos palabras de la marca de tu diario, como { $literal1 }.

linux.shared.pair.notpaired = sin vincular.

linux.shared.pair.setupcantaskyouaboutyourjournalsmarkherer = la configuración inicial no puede preguntarte por la marca de tu diario aquí. ejecútala en una terminal, o da las dos palabras de la marca con --mark "word word".

linux.shared.pair.thetwowordsofthemarkyourjournalsnetworkapp = las dos palabras de la marca que muestra la app de red de tu diario. se necesitan cuando no hay una terminal en la que preguntarte.

linux.shared.pair.typecontinuetopairanywayorcanceltostop = escribe { $literal1 } para vincular de todos modos, o { $literal2 } para detener:{ $ws3 }

linux.shared.pair.waitingforyoutoconfirmyourjournalsmarknothi = en espera de que confirmes la marca de tu diario. nada de lo que está en espera entra a tu diario hasta que lo hagas.

linux.shared.pair.yourjournalsmarkisalreadyconfirmednothingto = la marca de tu diario ya está confirmada. nada que hacer.

linux.shared.tray.offline = sin conexión

linux.sharedsources.also = también en

linux.sharedsources.cli = Obsidian: { $state }

linux.sharedsources.many = de otros { $count } dispositivos

linux.sharedsources.one = desde otro dispositivo

linux.sharedsources.pair = { $first } · { $second }

linux.shell.activitystillon = la actividad sigue activada.

linux.shell.afternoon = buenas tardes

linux.shell.amountsince = { $total }, desde { $date }

linux.shell.attention = necesita atención

linux.shell.browserunavailable = las páginas del navegador no pueden iniciarse en este momento.

linux.shell.checking = revisando…

linux.shell.evening = buenas noches

linux.shell.hourcount = { $count ->
    [one] { $display } hora
    [many] { $display } horas
   *[other] { $display } horas
}

linux.shell.hoursago = { $count ->
    [one] hace { $display } hora
    [many] hace { $display } horas
   *[other] hace { $display } horas
}

linux.shell.intakeunreadable = no se pudo leer el registro de cuánto se ha recibido.

linux.shell.itemcount = { $count ->
    [one] { $display } elemento
    [many] { $display } elementos
   *[other] { $display } elementos
}

linux.shell.lessthanminute = menos de un minuto

linux.shell.lessthanminuteleft = en pausa, queda menos de un minuto

linux.shell.minutecount = { $count ->
    [one] { $display } minuto
    [many] { $display } minutos
   *[other] { $display } minutos
}

linux.shell.minutesago = hace { $minutes } min

linux.shell.month.april = abril

linux.shell.month.august = agosto

linux.shell.month.december = diciembre

linux.shell.month.february = febrero

linux.shell.month.january = enero

linux.shell.month.july = julio

linux.shell.month.june = junio

linux.shell.month.march = marzo

linux.shell.month.may = mayo

linux.shell.month.november = noviembre

linux.shell.month.october = octubre

linux.shell.month.september = septiembre

linux.shell.monthday = { $month } { $day }

linux.shell.monthdayyear = { $day } { $month } { $year }

linux.shell.morning = buenos días

linux.shell.notecount = { $count ->
    [one] { $display } nota
    [many] { $display } notas
   *[other] { $display } notas
}

linux.shell.pagecount = { $count ->
    [one] { $display } página
    [many] { $display } páginas
   *[other] { $display } páginas
}

linux.shell.pauseleft = en pausa, quedan { $minutes } min

linux.shell.readytosetup = listo para configurar

linux.shell.screenmissingplugin = falta un complemento de { $literal1 }.

linux.shell.screensharingnotallowed = no se permitió compartir la pantalla.

linux.shell.screenshotcount = { $count ->
    [one] { $display } captura de pantalla
    [many] { $display } capturas de pantalla
   *[other] { $display } capturas de pantalla
}

linux.shell.screenstartfailed = no se pudo iniciar la recepción de pantalla.

linux.shell.settingup = configurando

linux.shell.terminalunavailable = las sesiones de terminal no pueden iniciarse en este momento.

linux.shell.tileneedsattention = { $source } necesita atención

linux.shell.untilresume = en pausa hasta que reanudes

linux.shell.withaudio = con audio

linux.shell.withaudioactivity = con audio y actividad

linux.shell.yesterday = ayer

linux.state.stoppedbyanerrorrestarting = se detuvo por un error, reiniciando

linux.state.terminalisoff = la terminal está desactivada

linux.state.tmuxnotfound = no se encontró { $literal1 }

linux.state.waitingsolstonefortmuxisstillinstalled = en espera: solstone para { $literal1 } sigue instalado. activa la terminal en la app, o ejecuta `{ $literal2 } terminal on`, para incorporarlo

linux.status.browsernobrowserconnected = navegador: ningún navegador conectado

linux.status.browserthesolstoneappisnotrunning = navegador: la app de solstone no está en ejecución

linux.status.heldxsegmentsnotsent = Retenidos: { $value1 } segmento(s) sin enviar

linux.status.heldxsegmentsnotsentoldestxd = Retenidos: { $value1 } segmento(s) sin enviar, el más antiguo { $value2 }d

linux.status.pausedsononewpagesaretaken = { $ws1 }en pausa, así que no se reciben páginas nuevas

linux.status.theroomforbrowserpagesonthis = { $ws1 }el espacio para las páginas del navegador en esta computadora está lleno, así que no se reciben páginas nuevas hasta que algunas pasen a tu diario

linux.sync.checkingjournal = revisando el diario...

linux.sync.checkingx = revisando { $day }...

linux.sync.probefailednextinxs = { $count ->
        [one] el sondeo falló, el siguiente en { $count } segundo
        [many] el sondeo falló, el siguiente en { $count } segundos
       *[other] el sondeo falló, el siguiente en { $count } segundos
    }

linux.sync.probingjournal = sondeando el diario...

linux.sync.syncing = sincronizando...

linux.sync.uploadingx = subiendo { $segment-key }

linux.sync.xsuntilprobe = { $count ->
        [one] { $count } segundo hasta el sondeo
        [many] { $count } segundos hasta el sondeo
       *[other] { $count } segundos hasta el sondeo
    }

linux.synchealth.ifyourecentlyresetjournal = si restableciste tu diario hace poco, vincula este dispositivo de nuevo.

linux.synchealth.thisislesscertainthan = esto tiene menos certeza que una conexión directa.

linux.terminal.control.bringold = pasando lo que está en espera

linux.terminal.control.confirmationsolstoneappisnt = { $confirmation } la app de solstone no está en ejecución; esto aplica cuando se inicie.

linux.terminal.control.couldnotmovesolstonetmux = no se pudo incorporar solstone para { $literal1 }: { $error }

linux.terminal.control.couldnotreachsolstoneapp = no se pudo conectar con la app de solstone: { $error }

linux.terminal.control.couldnotrecordmoveyet = todavía no se pudo registrar la incorporación: { $error }

linux.terminal.control.couldnotsavethaterror = no se pudo guardar eso: { $error }

linux.terminal.control.couldntuseproblem = { $ws1 }no se pudo usar { $value1 }: { $problem }

linux.terminal.control.ignoredsettingkey = { $ws1 }configuración ignorada: { $key }

linux.terminal.control.isoff = la terminal está desactivada.

linux.terminal.control.ison = la terminal está activada.

linux.terminal.control.morethanoneoldpairing = { $ws1 }más de una vinculación antigua: { $value1 }

linux.terminal.control.moving = incorporando solstone para tmux.

linux.terminal.control.movingsolstonetmuxneedsok = incorporar solstone para { $literal1 } necesita tu confirmación. ejecuta esto en una terminal, o agrega --yes a `terminal adopt-{ $literal1 }`.

linux.terminal.control.movingsolstonetmuxphase = { $ws1 }incorporando solstone para { $literal2 }: { $phase }

linux.terminal.control.newterminalhistoryfromhere = { $ws1 }nuevo historial de terminal desde aquí, y lo que ya está en tu diario se queda ({ $reason })

linux.terminal.control.nosolstonetmuxmove = no hay solstone para { $literal1 } que incorporar.

linux.terminal.control.oldsegmentsheldback = { $ws1 }{ $value1 } segmento(s) antiguo(s) retenido(s)

linux.terminal.control.problem = { $ws1 }problema: { $value1 }

linux.terminal.control.recordmovingsolstonetmuxcan = no se puede leer el registro de la incorporación de solstone para { $literal1 }: { $value1 }. contacta a soporte, por favor, y no lo elimines.

linux.terminal.control.screenaudiorestartappafter = pantalla y audio: reinicia la app después de iniciar sesión en tu escritorio aquí

linux.terminal.control.screenisoff = la pantalla, el audio y la actividad del escritorio están desactivados.

linux.terminal.control.screenison = la pantalla, el audio y la actividad del escritorio están activados.

linux.terminal.control.screenoffstatus = pantalla, audio y actividad del escritorio: desactivados

linux.terminal.control.segmentsfromsolstonetmux = { $ws1 }{ $value1 } segmento(s) de solstone para { $literal2 } en espera de subir

linux.terminal.control.segmentsheld = { $ws1 }{ $value1 } segmento(s) retenido(s)

linux.terminal.control.segmentswaitingupload = { $ws1 }{ $value1 } segmento(s) en espera de subir

linux.terminal.control.solstoneappdidnotanswer = la app de solstone no respondió a tiempo; vuelve a intentarlo en un momento

linux.terminal.control.solstoneappdidnotanswer2 = la app de solstone no respondió, así que no cambió nada; vuelve a intentarlo en un momento.

linux.terminal.control.solstoneappisstartingtry = la app de solstone se está iniciando; vuelve a intentarlo en un momento.

linux.terminal.control.solstoneappisstillmoving = la app de solstone todavía está incorporando solstone para { $literal1 }; termina por su cuenta.

linux.terminal.control.solstoneappneedsrestartingbefore = la app de solstone necesita reiniciarse antes de poder incorporar solstone para { $literal1 }.

linux.terminal.control.solstonetmuxhasalreadymoved = solstone para { $literal1 } ya se incorporó.

linux.terminal.control.solstonetmuxhasmoved = solstone para { $literal1 } se incorporó.

linux.terminal.control.solstonetmuxhasmovedsolstone = solstone para { $literal1 } se incorporó. la app de solstone termina el resto cuando se inicia.

linux.terminal.control.solstonetmuxisstillinstalled = solstone para { $literal1 } sigue instalado en esta computadora, así que la terminal se queda desactivada por ahora

linux.terminal.control.stopold = deteniendo la app anterior

linux.terminal.control.thismovessolstonetmuxinto = esto incorpora solstone para { $literal1 } en esta app, y ese solstone se detiene de forma definitiva. lo que todavía tiene en espera pasa a tu diario a través de esta app, y lo que no pueda pasar se queda en esta computadora. si esta app todavía no está vinculada, se vincula con el diario que usa solstone para { $literal1 }. ¿lo incorporas ahora? [y/N]

linux.terminal.control.tmux = { $ws1 }{ $literal2 }: { $value1 }

linux.terminal.control.unfinishedsegmentswaitingterminal = { $ws1 }{ $value1 } segmento(s) sin terminar en espera de que la terminal se vuelva a ejecutar

linux.terminal.control.waitinganswer = { $ws1 }en espera: { $answer }

linux.terminal.malformedcommand = comando mal formado

linux.terminal.state.stoppedbyerrorrestartingerror = se detuvo por un error, reiniciando: { $error }

linux.terminal.takeover.couldnotremoveerror = no se pudo quitar { $value1 }: { $error }

linux.terminal.takeover.isnotsolstonetmuxservice = { $value1 } no es el servicio de solstone para { $literal1 }, así que no cambió nada.

linux.terminal.takeoverrunner.couldnotsaveterminalsettings = no se pudo guardar la configuración de la terminal: { $error }

linux.terminal.takeoverrunner.couldnotturnterminalerror = no se pudo activar la terminal: { $error }

linux.terminal.warning = advertencia: { $detail }

linux.tray.aboutsolstone = acerca de solstone

linux.tray.cachexmb = caché: { $value1 } MB

linux.tray.cantcheckwhichsessionisinfront = no se puede revisar qué sesión está al frente

linux.tray.cantreachyourdesktop = no se puede conectar con tu escritorio

linux.tray.connectedallcaughtup = conectado · todo al día

linux.tray.idlescreeninactive = en reposo (pantalla inactiva)

linux.tray.n15minutes = 15 minutos

linux.tray.n1hour = 1 hora

linux.tray.n30minutes = 30 minutos

linux.tray.notpaired = sin vincular

linux.tray.notrunning = no está en ejecución

linux.tray.offlinewaiting = sin conexión · { $count } en espera

linux.tray.openjournal = abrir el diario

linux.tray.opensettings = abrir configuración

linux.tray.pausedxmremaining = { $count ->
        [one] en pausa ({ $count } minuto restante)
        [many] en pausa ({ $count } minutos restantes)
       *[other] en pausa ({ $count } minutos restantes)
    }

linux.tray.quitsolstone = salir de solstone

linux.tray.resume = reanudar

linux.tray.resumexmremaining = { $count ->
        [one] reanudar ({ $count } minuto restante)
        [many] reanudar ({ $count } minutos restantes)
       *[other] reanudar ({ $count } minutos restantes)
    }

linux.tray.screenintakecantstart = la recepción de pantalla no puede iniciarse

linux.tray.screenintakecouldntstart = no se pudo iniciar la recepción de pantalla

linux.tray.screenneedsattention = la pantalla necesita atención

linux.tray.screensharingnotallowed = no se permitió compartir la pantalla

linux.tray.tooltip = { $status }{ $break }{ $health }

linux.tray.untiliresume = hasta que yo reanude

linux.tray.waitingforscreenintake = en espera de la recepción de pantalla

linux.tray.waitingforyourdesktop = en espera de tu escritorio

linux.tray.waitingnoneofyoursessionsisinfront = en espera (ninguna de tus sesiones está al frente)

linux.tray.waitingyoursessionisntinfront = en espera (tu sesión no está al frente)

linux.ui.terminalterminalstatus = terminal: { $terminal-status }

linux.window.addmore = agregar más

linux.window.allcaughtup = todo al día

linux.window.brightharbor = bright · harbor

linux.window.browsers = navegadores

linux.window.browsersbreath = en los sitios que permites, la extensión de solstone recibe el texto y la disposición aproximada de la página, y todo va a tu diario.

linux.window.browsersgap = las páginas llegan desde la extensión de solstone. aquí no hay un interruptor para eso.

linux.window.cancel = cancelar

linux.window.checkingyourjournal = revisando tu diario.

linux.window.choosefiles = elegir archivos…

linux.window.choosenewdownloadname = elige un nombre nuevo para conservar el archivo existente

linux.window.close = cerrar

linux.window.connectajournal = conectar un diario

linux.window.connected = conectado

linux.window.connection = conexión

linux.window.everythinginyourjournal = todo está en tu diario

linux.window.files = archivos

linux.window.foregroundtitleurlidle = { $route }; primer plano { $foreground }; título { $title }; url { $url }; en reposo { $idle }

linux.window.goesalongwithit = va junto con esto

linux.window.help = ayuda

linux.window.home = ‹ inicio

linux.window.home.vcb176487 = inicio

linux.window.import = importar

linux.window.keptonthiscomputer = se conserva en esta computadora

linux.window.kilobytes = { $size } KB

linux.window.lastreached = última conexión

linux.window.lasttakenin = última recepción

linux.window.leftout = fuera

linux.window.megabytes = { $size } MB

linux.window.nothingyet = todavía no hay nada

linux.window.notpaired = sin vincular

linux.window.obsidian = { $literal1 }

linux.window.offsub = actívala cuando quieras.

linux.window.ok = ok

linux.window.onthisdevice = en este dispositivo

linux.window.pause = pausar ▾

linux.window.recent = recientes

linux.window.remove = quitar

linux.window.remove.vf022f0fa = ¿quitar { $value1 }?

linux.window.resume = reanudar

linux.window.screen = pantalla

linux.window.screenaudioandactivitystillon = el audio y la actividad siguen activados.

linux.window.screenaudiostillon = el audio sigue activado.

linux.window.screenbreath = mientras esto está activado, la app de solstone recibe lo que hay en tu pantalla, el sonido y qué aplicaciones y ventanas estás usando, y todo pasa a tu diario.

linux.window.screenloginunavailable = no se puede revisar qué sesión está al frente.

linux.window.screenmissingelements = la recepción de pantalla no puede iniciarse: a { $literal1 } le faltan { $elements }. instala { $literal1 } { $literal2 } y los complementos good y base con tu gestor de paquetes y luego elige reintentar.

linux.window.screenmissingelementsx11 = la recepción de pantalla no puede iniciarse: a { $literal1 } le faltan { $elements }. instala { $literal1 } y los complementos good y base con tu gestor de paquetes y luego elige reintentar.

linux.window.screenretrying = la recepción de pantalla no se inició al primer intento. se vuelve a intentar en un momento.

linux.window.screensharingnotallowed = no se permitió compartir la pantalla. elige reintentar para compartir tu pantalla.

linux.window.screensound = el sonido incluye tu micrófono y lo que reproduce esta computadora. si no hay micrófono, no se recibe ninguno de los dos.

linux.window.screenstartfailed = no se pudo iniciar la recepción de pantalla.

linux.window.screenunreachable = no se puede conectar con tu escritorio.

linux.window.screenwaiting = en espera de recibir la pantalla.

linux.window.sidebar = barra lateral

linux.window.sofar = hasta ahora

linux.window.somethingyouwereaddingdidntfinish = algo que estabas agregando no terminó de guardarse. agrégalo de nuevo.

linux.window.sources = fuentes

linux.window.status = estado

linux.window.status.vaff1290e = ‹ estado

linux.window.technicaldetails = detalles técnicos

linux.window.terminal = terminal

linux.window.terminalbreath = la app de solstone recibe tus sesiones de { $literal1 }, y todo pasa a tu diario. necesita { $literal1 } en esta computadora, y funciona en cualquier sesión de terminal, incluso cuando te conectas por { $literal2 } a esta computadora.

linux.window.thecopyisalreadygonesothis = la copia ya no está, así que esto solo la quita de aquí.

linux.window.thiscomputer = esta computadora

linux.window.thisremovesthecopyonthisdevice = esto quita la copia de este dispositivo. tu diario todavía puede recibir la copia que ya tiene.

linux.window.thisremovesthecopyonthisdevice.v558a0c4f = esto quita la copia de este dispositivo, así que no llegará a tu diario.

linux.window.trying = intentando…

linux.window.trynow = intentar ahora

linux.window.updates = actualizaciones

linux.window.yourjournal = tu diario

linux.window.yourjournal.vc43fd4d5 = tu · diario
