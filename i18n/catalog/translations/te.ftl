linux.about.ageday = { $count ->
    [one] { $formatted } రోజు క్రితం
   *[other] { $formatted } రోజుల క్రితం
}

linux.about.agehour = { $count ->
    [one] { $formatted } గంట క్రితం
   *[other] { $formatted } గంటల క్రితం
}

linux.about.ageminute = { $count ->
    [one] { $formatted } నిమిషం క్రితం
   *[other] { $formatted } నిమిషాల క్రితం
}

linux.about.app = linux డెస్క్‌టాప్ యాప్

linux.about.block = { $app }{ $break }{ $journal }

linux.about.journal = journal

linux.about.lastseen = { $journal } · చివరిసారి కనిపించింది { $age }

linux.about.now = ఇప్పుడే

linux.about.unknown = జర్నల్ తెలియదు

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

linux.bin.solstoneterminal.couldnotinstallsiginthandler = { $literal1 } హ్యాండ్లర్‌ను ఇన్‌స్టాల్ చేయలేకపోయింది: { $err }

linux.bin.solstoneterminal.couldnotinstallsigtermhandler = { $literal1 } హ్యాండ్లర్‌ను ఇన్‌స్టాల్ చేయలేకపోయింది: { $err }

linux.bin.solstoneterminal.executablemustbeabsolute = { $value1 }: ఎగ్జిక్యూటబుల్ సంపూర్ణంగా ఉండాలి

linux.bin.solstoneterminal.nothinghere = { $value1 }: ఇక్కడ ఏమీ లేదు

linux.bin.solstoneterminal.stillrunning = { $value1 }: ఇంకా నడుస్తోంది

linux.browser.status.browserconnected = బ్రౌజర్: { $value1 } కనెక్ట్ అయి ఉంది

linux.browser.status.browserpagesthiscomputerwaiting = { $ws1 }ఈ కంప్యూటర్‌లోని బ్రౌజర్ పేజీలలో { $value1 }, మీ జర్నల్‌లోకి వెళ్ళడానికి వేచి ఉంది

linux.browser.status.chromeedge = Chrome లేదా Edge

linux.browser.status.ifyouhaventyet = { $ws1 }ఇంకా చేర్చకపోతే, solstone ఎక్స్‌టెన్షన్‌ను { $served } లో చేర్చండి: { $url }

linux.browser.status.n0kb = { $value1 } KB

linux.browser.status.n1mb = { $value1 } MB

linux.cli.agreewithoutbeingasked = అడగకుండా అంగీకరించడం

linux.cli.anothercopyofthesolstoneappisalreadyrunning = ఈ లాగిన్ కోసం solstone యాప్ యొక్క మరో కాపీ ఇప్పటికే నడుస్తోంది, కాబట్టి ఇది స్టార్ట్ కాలేదు. ఆ మరో కాపీని ఆపండి, తర్వాత మళ్ళీ ప్రయత్నించండి.

linux.cli.browserendpointisnottrusted = బ్రౌజర్ endpoint నమ్మదగినది కాదు

linux.cli.cachex = కాష్:{ $ws1 }{ $value1 }

linux.cli.cachexnotcreatedyet = కాష్:{ $ws1 }{ $value1 } (ఇంకా ఏర్పడలేదు)

linux.cli.commandoutputexceededitsbound = కమాండ్ అవుట్‌పుట్ తన పరిమితిని దాటింది

linux.cli.commandtimedout = కమాండ్ టైమ్‌అవుట్ అయింది

linux.cli.configupdateerrorx = కాన్ఫిగ్ అప్‌డేట్ లోపం: { $error }

linux.cli.configx = కాన్ఫిగ్: { $value1 }

linux.cli.confirmyourjournalsmarkexitcodes0pairedand = మీ జర్నల్ మార్క్‌ను నిర్ధారించండి.{ $ws1 }ఎగ్జిట్ కోడ్‌లు:{ $ws2 }0{ $ws3 }పెయిర్ అయి నిర్ధారించబడింది, లేదా ఇప్పటికే నిర్ధారించబడింది{ $ws4 }1{ $ws5 }పెయిర్ కాలేదు, ఏమీ మారలేదు, లేదా టర్మినల్ లేదు{ $ws6 }2{ $ws7 }వాడక లోపం{ $ws8 }5{ $ws9 }నిలిచి ఉంది

linux.cli.couldnotdiscardthebrowserpagesx = బ్రౌజర్ పేజీలను వదలలేకపోయింది: { $error }

linux.cli.couldnotstartx = స్టార్ట్ చేయలేకపోయింది: { $error }

linux.cli.couldntkeepthatlinkforthe = ఆ విండో లింక్‌ను ఉంచలేకపోయింది: { $error }

linux.cli.credentialabsent = క్రెడెన్షియల్ లేదు

linux.cli.discardfailed = వదలడం విఫలమైంది

linux.cli.discardreplytoolarge = వదిలే సమాధానం చాలా పెద్దది

linux.cli.discardwaitingbrowserpages = వేచి ఉన్న బ్రౌజర్ పేజీలను వదలండి

linux.cli.drawcursor = కర్సర్ గీయండి

linux.cli.editsettings = సెట్టింగ్‌లు మార్చండి

linux.cli.enterapositiveinteger = 0 కంటే పెద్ద పూర్ణాంకం ఇవ్వండి.

linux.cli.enteryorn = { $literal1 } లేదా { $literal2 } ఇవ్వండి.

linux.cli.erroreditingsettingsx = సెట్టింగ్‌లు మార్చడంలో లోపం: { $error }

linux.cli.errorx = లోపం: { $error }

linux.cli.howmanyminutestopausefordefaultuntilyouresu = ఎన్ని నిమిషాలు పాజ్ చేయాలి (డిఫాల్ట్: మీరు కొనసాగించే వరకు)

linux.cli.installgnomeandkwinactivityhelperassets = { $command1 } మరియు { $command2 } కార్యకలాప సహాయకం assets ఇన్‌స్టాల్ చేయండి

linux.cli.installsystemduserservice = systemd యూజర్ సర్వీస్ ఇన్‌స్టాల్ చేయండి

linux.cli.invaliddiscardreply = సరైనది కాని వదిలే సమాధానం

linux.cli.journalversionx = journal వెర్షన్: { $version-str }

linux.cli.linkedprivatestateisalreadyinuse = లింక్ అయిన ప్రైవేట్ స్థితి ఇప్పటికే వాడుకలో ఉంది

linux.cli.missingexecutabledirectory = ఎగ్జిక్యూటబుల్ డైరెక్టరీ లేదు

linux.cli.moveexistingsolstonetmuxinto = ఈ యాప్‌లోకి ఉన్న solstone for { $command0 } తరలించండి

linux.cli.nosessionbussothereisnodesktoptosetupx = సెషన్ బస్ లేదు, కాబట్టి సెటప్ చేసే డెస్క్‌టాప్ లేదు: { $error }

linux.cli.notset = (పెట్టలేదు)

linux.cli.pairthisdevicewithyourjournal = ఈ పరికరాన్ని మీ జర్నల్‌తో పెయిర్ చేయండి

linux.cli.pairthisdevicewithyourjournalexitcodes0pair = ఈ పరికరాన్ని మీ జర్నల్‌తో పెయిర్ చేయండి.{ $ws1 }ఎగ్జిట్ కోడ్‌లు:{ $ws2 }0{ $ws3 }పెయిర్ అయి నిర్ధారించబడింది, లేదా ఇప్పటికే నిర్ధారించబడింది{ $ws4 }1{ $ws5 }పెయిర్ కాలేదు, ఏమీ మారలేదు, లేదా టర్మినల్ లేదు{ $ws6 }2{ $ws7 }వాడక లోపం{ $ws8 }5{ $ws9 }నిలిచి ఉంది

linux.cli.pauseintake = తీసుకోలు పాజ్ చేయండి

linux.cli.resumeintake = తీసుకోలు కొనసాగించండి

linux.cli.screenaudiodesktopactivityoff = స్క్రీన్, ఆడియో మరియు డెస్క్‌టాప్ కార్యకలాపం: ఆన్ లేదా ఆఫ్

linux.cli.segmentdurationinsecondsdefault300 = సెగ్మెంట్ వ్యవధి సెకన్లలో (డిఫాల్ట్: 300)

linux.cli.segmentintervalseconds = సెగ్మెంట్ విరామం సెకన్లు

linux.cli.servicex = { $ws1 }సర్వీస్: { $value1 }

linux.cli.settingsboolprompt = { $label } [{ $answer }]:{ $ws1 }

linux.cli.settingsintprompt = { $label } [{ $current }]:{ $ws1 }

linux.cli.settingssavedtox = { $ws1 }సెట్టింగ్‌లు ఇక్కడ సేవ్ అయ్యాయి: { $value1 }

linux.cli.setupappalreadyrunning = solstone యాప్ నడుస్తోంది కాబట్టి సెటప్ స్టార్ట్ కాలేదు. ముందు solstone యాప్‌ను ఆపి మళ్ళీ ప్రయత్నించండి. ఇన్‌పుట్ ఏదీ తీసుకోలేదు. యాప్ స్థితి, కాన్ఫిగ్, ప్రైవేట్ స్థితి మారలేదు.

linux.cli.setupconfigfailed = పెయిరింగ్‌కు ముందే సెటప్ సాధ్యం కాలేదు, ఎందుకంటే solstone యాప్ తన కాన్ఫిగ్‌ను సురక్షితంగా అప్‌డేట్ చేయలేకపోయింది.

linux.cli.setupconnectionfailed = సెటప్ సాధ్యం కాలేదు: solstone యాప్ మీ జర్నల్‌కు కనెక్ట్ కాలేదు.

linux.cli.setupfailedx = సెటప్ సాధ్యం కాలేదు: { $error }

linux.cli.setupthegnomepaneliconwherepauseandresumel = { $command1 } ప్యానెల్ ఐకాన్‌ను సెటప్ చేయండి. పాజ్ మరియు కొనసాగించండి అక్కడ ఉంటాయి.

linux.cli.showstatus = స్థితి చూపండి

linux.cli.showwhetherterminaliswhat = టర్మినల్ ఆన్‌లో ఉందా, ఏమి చేస్తోందో చూపండి

linux.cli.showyourappandjournalversions = మీ యాప్ మరియు journal వెర్షన్‌లు చూపండి

linux.cli.startpaused = పాజ్‌తో ప్రారంభించండి

linux.cli.startupcouldnotcontinuebecausethesolstoneapp = మునుపటి రన్ సింక్ స్థితిని solstone యాప్ తీసివేయలేకపోయింది, కాబట్టి స్టార్ట్ కొనసాగలేదు. solstone యాప్ ఈ కంప్యూటర్‌లో తన డేటాను రాయగలదో సరిచూడండి, తర్వాత మళ్ళీ ప్రయత్నించండి.

linux.cli.startupcouldnotcontinuebecausethesolstoneapp2 = solstone యాప్ ఈ కంప్యూటర్‌లో తన డేటాను సురక్షితంగా సిద్ధం చేయలేకపోయింది, కాబట్టి స్టార్ట్ కొనసాగలేదు. solstone యాప్ ఆ డేటాను రాయగలదో సరిచూడండి, తర్వాత మళ్ళీ ప్రయత్నించండి.

linux.cli.streamnamedefaultstohostnamederived = ప్రవాహం పేరు (డిఫాల్ట్: హోస్ట్ పేరు నుండి)

linux.cli.streamx = ప్రవాహం: { $stream }

linux.cli.terminaloffstatus = టర్మినల్: { $command3 } లేదా స్థితి

linux.cli.thesetakeeffectthenexttimethesolstoneappsta = ఇవి solstone యాప్ తర్వాతి సారి స్టార్ట్ అయినప్పుడు అమలు అవుతాయి.

linux.cli.thesolstoneappforlinuxtakesinwhatyousharew = linux కోసం solstone యాప్ మీరు దానితో పంచుకున్నది స్వీకరిస్తుంది, అంతా మీ జర్నల్‌లోకి వెళ్తుంది. solstoneలో భాగం.

linux.cli.turnterminal = టర్మినల్ ఆన్ చేయండి

linux.cli.turnterminaloff = టర్మినల్ ఆఫ్ చేయండి

linux.cli.uninstallsystemduserservice = systemd యూజర్ సర్వీస్ అన్‌ఇన్‌స్టాల్ చేయండి

linux.cli.verifyinstallprerequisites = ఇన్‌స్టాల్ ముందు అవసరాలను ధృవీకరించండి

linux.cli.windowscopeexitedx = విండో స్కోప్ ముగిసింది: { $status }

linux.cli.xincompletesegments = { $ws1 }{ $value1 } పూర్తి కాని సెగ్మెంట్(లు)

linux.cli.xsegmentsacrossxdaysxmb = { $ws1 }{ $value1 } సెగ్మెంట్‌లు, { $value2 } రోజు(లు)లో, { $value3 } MB

linux.clipboard.agentinstructions = linux కోసం solstone యాప్ (repo: { $repo })
    సోర్స్: { $repository }
    సెటప్ మరియు ఆర్కిటెక్చర్ కోసం { $install } వద్ద INSTALL.md చదవండి.
    కాన్ఫిగ్: { $config_path }
    సెగ్మెంట్‌లు: { $captures_dir }
    లాగ్‌లు: { $logs }
    సర్వీస్: { $service }

linux.control.screenandaudioneedyourdesktopsession = స్క్రీన్ మరియు ఆడియో: మీ డెస్క్‌టాప్ సెషన్ కావాలి, ఏదీ నడవడం లేదు

linux.control.solstonefortmuxhadbeenstoppedso = { $ws1 }solstone for { $literal2 } ఆగిపోయి ఉంది, కాబట్టి టర్మినల్ ఆఫ్‌లోనే ఉంటుంది

linux.control.terminalhistorycontinued = { $ws1 }టర్మినల్ చరిత్ర: కొనసాగింది

linux.control.terminalhistorycontinuedundertheoldpairing = { $ws1 }టర్మినల్ చరిత్ర: కొనసాగింది, పాత పెయిరింగ్ కింద

linux.control.thesolstoneappdidnotanswerin = solstone యాప్ సమయానికి సమాధానం ఇవ్వలేదు. ఒక క్షణం తర్వాత మళ్ళీ ప్రయత్నించండి.

linux.control.thesolstoneappisntrunning = { $ws1 }solstone యాప్ నడవడం లేదు

linux.control.thesolstoneappisntrunningstart = solstone యాప్ నడవడం లేదు. దాన్ని స్టార్ట్ చేసి మళ్ళీ ప్రయత్నించండి.

linux.control.thesolstoneappisstartingtryagain = solstone యాప్ స్టార్ట్ అవుతోంది. ఒక క్షణం తర్వాత మళ్ళీ ప్రయత్నించండి.

linux.control.thisneedsattentionpleasecontactsupport = { $ws1 }దీనికి శ్రద్ధ కావాలి: దయచేసి సపోర్ట్‌ను సంప్రదించండి

linux.copy.aboutheading = solstone గురించి

linux.copy.aboutmadeby = { $literal1 } చేసింది.

linux.copy.activityonly = కార్యకలాపం మాత్రమే

linux.copy.activityroute = కార్యకలాప మార్గం

linux.copy.anythingwaitingsyncstaysthis = సింక్ కోసం వేచి ఉన్నదంతా ఈ కంప్యూటర్‌లోనే ఉంటుంది. ఏమి చేయాలో చూడటానికి ఇది రన్ చేయండి: { $literal1 }

linux.copy.anythingwaitingtosyncstaysonthiscomputerthe = సింక్ కోసం వేచి ఉన్నదంతా ఈ కంప్యూటర్‌లోనే ఉంటుంది. మీ జర్నల్ అందుబాటులో ఉన్నప్పుడు solstone యాప్ మళ్ళీ ప్రయత్నిస్తుంది.

linux.copy.appsleftoutofdesktopactivity = డెస్క్‌టాప్ కార్యకలాపం నుండి మినహాయించిన యాప్‌లు

linux.copy.appversion = యాప్ వెర్షన్

linux.copy.attentioncount = { $count ->
    [one] { $formatted } శ్రద్ధ కావాలి
   *[other] { $formatted } శ్రద్ధ కావాలి
}

linux.copy.audio = ఆడియో

linux.copy.audiointakecancontinue = ఆడియో తీసుకోలు కొనసాగవచ్చు.

linux.copy.availablelogsareshownbelowcheckthenotesforg = అందుబాటులో ఉన్న లాగ్‌లు కింద చూపబడ్డాయి. కాపీ సేవ్ చేయడానికి ముందు, ఖాళీల కోసం నోట్‌లు సరిచూడండి.

linux.copy.cancelpairing = పెయిరింగ్ రద్దు చేయండి

linux.copy.checkingstatus = స్థితి సరిచూస్తోంది…

linux.copy.checkstatus = స్థితిని సరిచూడండి

linux.copy.choose = ఎంచుకోండి

linux.copy.choosefolder = ఒక ఫోల్డర్‌ను ఎంచుకోండి

linux.copy.choosefolder.v4cba918b = ఫోల్డర్‌ను ఎంచుకోండి…

linux.copy.chooseobsidianvault = మీ { $literal1 } వాల్ట్‌ను ఎంచుకోండి

linux.copy.configfile = కాన్ఫిగ్ ఫైల్

linux.copy.confirming = నిర్ధారిస్తోంది…

linux.copy.confirmyourjournalsmark = మీ జర్నల్ మార్క్‌ను నిర్ధారించండి

linux.copy.connected = కనెక్ట్ అయి ఉంది

linux.copy.connecting = కనెక్ట్ అవుతోంది…

linux.copy.connectjournal = మీ జర్నల్‌తో కనెక్ట్ అవ్వండి

linux.copy.copysaved = కాపీ సేవ్ అయింది.

linux.copy.couldntcheckwhetherallyoursettingsweresaved = మీ సెట్టింగ్‌లన్నీ సేవ్ అయ్యాయో సరిచూడలేకపోయింది. మళ్ళీ ప్రయత్నించే ముందు వాటిని మళ్ళీ చదవండి.

linux.copy.couldntcheckwhetherintakepausedcheckstatusbe = తీసుకోలు పాజ్ అయిందో సరిచూడలేకపోయింది. మళ్ళీ ప్రయత్నించే ముందు స్థితిని సరిచూడండి.

linux.copy.couldntcheckwhetherintakeresumedcheckstatusb = తీసుకోలు కొనసాగిందో సరిచూడలేకపోయింది. మళ్ళీ ప్రయత్నించే ముందు స్థితిని సరిచూడండి.

linux.copy.couldntcheckwhetherpairingfinishedcheckstatus = పెయిరింగ్ పూర్తయిందో సరిచూడలేకపోయింది. మళ్ళీ ప్రయత్నించే ముందు స్థితిని సరిచూడండి.

linux.copy.couldntopenthatlinkvisitsupportsolstoneappo = ఆ లింక్‌ను తెరవలేకపోయింది. { $literal1 } చూడండి, లేదా ఇమెయిల్ పంపండి: { $literal2 }.

linux.copy.couldntopenthesolstonewindow = solstone విండోను తెరవలేకపోయింది

linux.copy.couldntopenthesolstonewindowopenxfordetails = solstone విండోను తెరవలేకపోయింది. వివరాల కోసం { $path } తెరవండి.

linux.copy.couldntopenyourbrowsertryagainoropenyourjo = మీ బ్రౌజర్‌ను తెరవలేకపోయింది. మళ్ళీ ప్రయత్నించండి, లేదా మీ జర్నల్‌ను దాని మామూలు చిరునామా నుండి తెరవండి.

linux.copy.couldntpauseintaketryagain = తీసుకోలు పాజ్ చేయలేకపోయింది. మళ్ళీ ప్రయత్నించండి.

linux.copy.couldntreadthelogfilestryagain = లాగ్ ఫైల్‌లు చదవలేకపోయింది. మళ్ళీ ప్రయత్నించండి.

linux.copy.couldntreadthelogstryagain = లాగ్‌లు చదవలేకపోయింది. మళ్ళీ ప్రయత్నించండి.

linux.copy.couldntreadthesolstoneappsstatuscheckwhethe = solstone యాప్ స్థితిని చదవలేకపోయింది. అది నడుస్తోందో సరిచూసి, మళ్ళీ ప్రయత్నించండి.

linux.copy.couldntreadyoursettingstryagainbeforemaking = మీ సెట్టింగ్‌లను చదవలేకపోయింది. మార్పులు చేయడానికి ముందు మళ్ళీ ప్రయత్నించండి.

linux.copy.couldntrestartthesolstoneappcheckthedetails = solstone యాప్‌ను రీస్టార్ట్ చేయలేకపోయింది. వివరాలు సరిచూసి, మళ్ళీ ప్రయత్నించండి.

linux.copy.couldntresumeintaketryagain = తీసుకోలు కొనసాగించలేకపోయింది. మళ్ళీ ప్రయత్నించండి.

linux.copy.couldntsaveactivityintaketryagain = కార్యకలాపం తీసుకోలును సేవ్ చేయలేకపోయింది. మళ్ళీ ప్రయత్నించండి.

linux.copy.couldntsaveallyoursettingsyoureditsarestill = మీ సెట్టింగ్‌లన్నీ సేవ్ చేయలేకపోయింది. మీ మార్పులు ఇక్కడే ఉన్నాయి. మళ్ళీ ప్రయత్నించండి.

linux.copy.couldntsavethecopytrysavingtoanotherfolder = కాపీ సేవ్ చేయలేకపోయింది. ఇంకో ఫోల్డర్‌కు సేవ్ చేయండి.

linux.copy.couldntsaveyouranswertryagain = మీ సమాధానాన్ని సేవ్ చేయలేకపోయింది. మళ్ళీ ప్రయత్నించండి.

linux.copy.couldntsaveyoursettingscheckthedetailsthent = మీ సెట్టింగ్‌లను సేవ్ చేయలేకపోయింది. వివరాలు సరిచూసి, మళ్ళీ ప్రయత్నించండి.

linux.copy.couldntstartthesolstoneappcheckthedetailsth = solstone యాప్‌ను స్టార్ట్ చేయలేకపోయింది. వివరాలు సరిచూసి, మళ్ళీ ప్రయత్నించండి.

linux.copy.couldntverify = ధృవీకరించలేకపోయింది

linux.copy.couldntverifyjournalso = మీ జర్నల్‌ను ధృవీకరించలేకపోయింది, కాబట్టి ఏదీ కనెక్ట్ కాలేదు. మళ్ళీ ప్రయత్నించండి, లేదా పెయిరింగ్ లింక్‌ను పేస్ట్ చేయండి.

linux.copy.couldntworkoutthismarksotheresnothingtoco = ఈ మార్క్‌ను అర్థం చేసుకోలేకపోయింది, కాబట్టి పోల్చడానికి ఏమీ లేదు. మీ ప్రస్తుత పెయిరింగ్ వాడుకలోనే ఉంటుంది.

linux.copy.couldntworkoutyourjournalsmarksotheresnot = ఆ జర్నల్ మార్క్‌ను అర్థం చేసుకోలేకపోయింది, కాబట్టి పోల్చడానికి ఏమీ లేదు. మీ జర్నల్ నుండి కొత్త పెయిరింగ్ లింక్ తీసుకుని, మళ్ళీ ప్రయత్నించండి.

linux.copy.countwaitingsync = { $count } సింక్ కోసం వేచి ఉంది

linux.copy.d1 = ఇది ఆన్‌లో ఉన్నప్పుడు, ఈ కంప్యూటర్‌లో మీరు తీసే స్క్రీన్‌షాట్‌లను solstone యాప్ స్వీకరిస్తుంది, మీరు తీసిన నిమిషానికి మీ జర్నల్ ఒక్కోదాన్ని ఉంచుతుంది.

linux.copy.d10 = కొన్ని స్క్రీన్‌షాట్ టూల్స్ మీరు అడిగినప్పుడే సేవ్ చేస్తాయి, సేవ్ కాని స్క్రీన్‌షాట్ లోపలికి రాదు.

linux.copy.d15 = దీని గురించి మరింత

linux.copy.d8 = ఈ కంప్యూటర్‌లో Screenshots ఫోల్డర్ కనబడలేదు. మీ స్క్రీన్‌షాట్‌లు సేవ్ అయ్యే ఫోల్డర్‌ను ఎంచుకోండి.

linux.copy.d8pickertitle = మీ స్క్రీన్‌షాట్‌లు సేవ్ అయ్యే ఫోల్డర్ ఎంచుకోండి

linux.copy.desktopactivity = డెస్క్‌టాప్ కార్యకలాపం

linux.copy.desktopactivityisonwheneverthesolstoneapptak = మీ స్క్రీన్‌ను solstone యాప్ స్వీకరించినప్పుడల్లా డెస్క్‌టాప్ కార్యకలాపం ఆన్‌లో ఉంటుంది, పాజ్ చేస్తే రెండూ పాజ్ అవుతాయి. మీరు పంచుకున్న యాప్, విండో శీర్షిక, వెబ్ చిరునామాలు, మరియు యాక్టివ్‌గా ఉన్న లేదా పని లేని సమయం solstone యాప్ స్వీకరిస్తుంది, అంతా మీ జర్నల్‌లోకి వెళ్తుంది.

linux.copy.doesthismatchyourjournal = ఇది మీ జర్నల్‌తో సరిపోతుందా?

linux.copy.drawcursor = కర్సర్ గీయండి

linux.copy.enterawholenumbergreaterthan0 = 0 కంటే పెద్ద పూర్ణాంకం ఇవ్వండి.

linux.copy.foundjournalthiscomputer = ఈ కంప్యూటర్‌లో మీ జర్నల్ కనిపించింది

linux.copy.from = { $folder } నుండి

linux.copy.getafreshlink = తాజా లింక్ తీసుకోండి

linux.copy.gethelp = సహాయం పొందండి

linux.copy.healthdetails = స్థితి వివరాలు

linux.copy.help = సహాయం

linux.copy.helpreport = సమస్యను నివేదించండి

linux.copy.intake = తీసుకోలు

linux.copy.intakeisntrunning = తీసుకోలు నడవడం లేదు.

linux.copy.intakeispauseduntilyouresume = మీరు కొనసాగించే వరకు తీసుకోలు పాజ్ అయి ఉంది.

linux.copy.intakeisrunning = తీసుకోలు నడుస్తోంది.

linux.copy.journal = journal

linux.copy.journal3 = మీ జర్నల్

linux.copy.journaldetails = జర్నల్ వివరాలు

linux.copy.journallinkinvalidbody = మీ చాట్ నుండి పూర్తి లింక్ కాపీ చేసి మళ్ళీ తెరవండి.

linux.copy.journallinkinvalidtitle = ఈ జర్నల్ లింక్ చెల్లదు.

linux.copy.journalneedsupdatebeforeit = ఈ నోట్‌లు స్వీకరించడానికి ముందు మీ జర్నల్‌కు అప్‌డేట్ కావాలి. అవి ఈ కంప్యూటర్‌లో వేచి ఉన్నాయి.

linux.copy.journalsetupaction = ఈ కంప్యూటర్‌లో ఒక journal సెటప్ చేయండి

linux.copy.journalsetupfailed = మీ జర్నల్‌ను సిద్ధం చేయలేకపోయింది. టర్మినల్ లో ఇది రన్ చేయండి: { $literal1 }. దేనికి శ్రద్ధ కావాలో అది చూపిస్తుంది.

linux.copy.journalsetupinbrowser = మీ బ్రౌజర్‌లో సెటప్ పూర్తి చేయండి.

linux.copy.journalsetupinstall = journal ఇన్‌స్టాల్ చేయండి

linux.copy.journalsetupnotinstalled = ఈ కంప్యూటర్‌లో journal ఇంకా ఇన్‌స్టాల్ కాలేదు.

linux.copy.journalsetupopenfailed = మీ జర్నల్ సిద్ధంగా ఉంది, కానీ బ్రౌజర్ తెరవలేదు. సెటప్ పూర్తి చేయడానికి టర్మినల్ లో ఇది రన్ చేయండి: { $literal1 }

linux.copy.journalsetuppreparing = మీ జర్నల్ సిద్ధమవుతోంది…

linux.copy.keepdesktopactivityonwhilepaused = పాజ్ అయి ఉన్నప్పుడు డెస్క్‌టాప్ కార్యకలాపం ఆన్‌లో ఉంచండి

linux.copy.lastchecked = చివరిసారి సరిచూసింది

linux.copy.lastdelivery = చివరి చేరవేత

linux.copy.lastdeliveryvalue = చివరి చేరవేత: { $value }

linux.copy.linuxdoesnotclassifyprivatewindows = linux ప్రైవేట్ విండోలను వర్గీకరించదు.

linux.copy.mark = మార్క్

linux.copy.markunavailable = మార్క్ అందుబాటులో లేదు

linux.copy.moveit = దీన్ని తరలించండి

linux.copy.movesolstonetmux = solstone for { $literal1 } లోపలికి తరలించాలా?

linux.copy.needahandreachahumanwerehappytohelp = సహాయం కావాలా? మమ్మల్ని సంప్రదించండి. సహాయం చేయడానికి సంతోషంగా ఉన్నాం.

linux.copy.needattention = శ్రద్ధ కావాలి

linux.copy.needsattention = శ్రద్ధ కావాలి

linux.copy.newpairingcancelled = కొత్త పెయిరింగ్ రద్దు అయింది

linux.copy.newpairingcancelledyourcurrentpairingstaysin = కొత్త పెయిరింగ్ రద్దు అయింది. మీ ప్రస్తుత పెయిరింగ్ వాడుకలోనే ఉంటుంది.

linux.copy.nologfilesareavailableyettryagainafterusing = ఇంకా లాగ్ ఫైల్‌లు అందుబాటులో లేవు. యాప్ వాడిన తర్వాత మళ్ళీ ప్రయత్నించండి.

linux.copy.norecentlogsareavailableyettryagainafterusi = ఇటీవలి లాగ్‌లు ఇంకా అందుబాటులో లేవు. యాప్ వాడిన తర్వాత మళ్ళీ ప్రయత్నించండి.

linux.copy.not = కాదు

linux.copy.nothingthiscomputerhaskept = ఈ లింక్ ద్వారా, ఈ కంప్యూటర్ ఉంచినది ఏదీ ఆ జర్నల్‌కు వెళ్ళలేదు. మీరు తప్పు లింక్ పేస్ట్ చేసి ఉండవచ్చు, లేదా ఏదో సరిగా లేదు. మీ జర్నల్ నుండి కొత్త పెయిరింగ్ లింక్ తీసుకుని మళ్ళీ ప్రయత్నించండి, లేదా ఇమెయిల్ పంపండి: { $literal1 }. మేము సహాయం చేస్తాము.

linux.copy.nothingwaitinggoesintoyourjournaluntilyoudo = మీరు చేసే వరకు వేచి ఉన్నది ఏదీ మీ జర్నల్‌లోకి వెళ్లదు.

linux.copy.notpaired = పెయిర్ కాలేదు

linux.copy.notreachingyourjournalrightnow = ఇప్పుడు మీ జర్నల్‌కు చేరడం లేదు

linux.copy.notthisdevice = ఈ పరికరంలో లేదు

linux.copy.notyet = ఇంకా కాదు

linux.copy.obsidian = { $literal1 }

linux.copy.obsidianwarning = మీ జర్నల్‌లో ఇంకో పరికరం నుండి { $vault } పేరుతో వాల్ట్ ఇప్పటికే ఉంది. కొనసాగిస్తే, రెండు పరికరాల మార్పులు మీ జర్నల్‌లోకి వెళ్తాయి.

linux.copy.obsidianwarningdate = మీ జర్నల్‌లో ఇంకో పరికరం నుండి { $vault } పేరుతో వాల్ట్ ఇప్పటికే ఉంది, చివరిగా { $date } న అప్‌డేట్ అయింది. కొనసాగిస్తే, రెండు పరికరాల మార్పులు మీ జర్నల్‌లోకి వెళ్తాయి.

linux.copy.obsidianwarningdevice = మీ జర్నల్‌లో { $device } నుండి { $vault } పేరుతో వాల్ట్ ఇప్పటికే ఉంది. కొనసాగిస్తే, రెండు పరికరాల మార్పులు మీ జర్నల్‌లోకి వెళ్తాయి.

linux.copy.obsidianwarningdevicedate = మీ జర్నల్‌లో { $device } నుండి { $vault } పేరుతో వాల్ట్ ఇప్పటికే ఉంది, చివరిగా { $date } న అప్‌డేట్ అయింది. కొనసాగిస్తే, రెండు పరికరాల మార్పులు మీ జర్నల్‌లోకి వెళ్తాయి.

linux.copy.off = ఆఫ్

linux.copy.offmeansnonethreegoes = ఆఫ్ అంటే మూడూ మీ జర్నల్‌లోకి వెళ్ళవు, మీ డెస్క్‌టాప్ ఏమి చేస్తున్నా.

linux.copy.ofinyourjournal = మీ జర్నల్‌లో { $done } / { $total }

linux.copy.ofscreenshotsinyourjournal = మీ జర్నల్‌లో { $done } / { $total } స్క్రీన్‌షాట్‌లు

linux.copy.omittedbytes = { $count ->
    [one] { $break }[మినహాయించిన { $formatted } బైట్]{ $break }
   *[other] { $break }[మినహాయించిన { $formatted } బైట్‌లు]{ $break }
}

linux.copy.on = ఆన్

linux.copy.openthenetworkappinyourjournalandgetapairi = మీ జర్నల్‌లో నెట్‌వర్క్ యాప్ తెరిచి, పెయిరింగ్ లింక్ తీసుకోండి.

linux.copy.openyourjournal = మీ జర్నల్‌ను తెరవండి

linux.copy.otherfilesinstaywheretheyare = { $folder } లోని ఇతర ఫైల్‌లు అక్కడే ఉంటాయి.

linux.copy.pairadevice = ఒక పరికరాన్ని పెయిరింగ్ చేయండి

linux.copy.pairagain = మళ్ళీ పెయిరింగ్ చేయండి

linux.copy.pairing = పెయిర్ అవుతోంది…

linux.copy.pairingcancelledgetafreshpairinglinkfromyour = పెయిరింగ్ రద్దు అయింది. మీరు సిద్ధంగా ఉన్నప్పుడు మీ జర్నల్ నుండి కొత్త పెయిరింగ్ లింక్ తీసుకోండి.

linux.copy.pairingdidntgothroughgetanewpairinglinkfro = పెయిరింగ్ కాలేదు. మీ జర్నల్ నుండి కొత్త పెయిరింగ్ లింక్ తెచ్చుకుని మళ్ళీ ప్రయత్నించండి.

linux.copy.pairinglink = పెయిరింగ్ లింక్

linux.copy.partthisvaultcant = ఈ వాల్ట్‌లో కొంత భాగం చదవలేకపోయింది. దాని లోపలి ఫోల్డర్‌ల అనుమతులు సరిచూడండి. వాల్ట్ మిగతా భాగం మీ జర్నల్‌లోకి వెళ్తూనే ఉంటుంది.

linux.copy.pasteapairinglinkfromyourjournal = మీ జర్నల్ నుండి పెయిరింగ్ లింక్‌ను పేస్ట్ చేయండి.

linux.copy.pastepairinglink = పెయిరింగ్ లింక్‌ను పేస్ట్ చేయండి

linux.copy.pause = పాజ్ చేయండి

linux.copy.paused = పాజ్ అయి ఉంది

linux.copy.pausing = పాజ్ అవుతోంది…

linux.copy.presented = చూపించబడింది

linux.copy.quit = solstone నుండి నిష్క్రమించండి

linux.copy.quitfailed = solstone యాప్‌ను ఆపలేకపోయింది. మళ్ళీ ప్రయత్నించండి.

linux.copy.readinglogs = లాగ్‌లు చదువుతోంది…

linux.copy.readrecentlogs = ఇటీవలి లాగ్‌లు చదవండి

linux.copy.readsettingsagain = సెట్టింగ్‌లు మళ్ళీ చదవండి

linux.copy.readthelogsbelowthenchoosewhethertosaveaco = కింద లాగ్‌లు చదవండి, తర్వాత కాపీ సేవ్ చేయాలా అని ఎంచుకోండి.

linux.copy.readthesolstoneappsrecentlogsbeforesavinga = సపోర్ట్ నివేదిక కోసం కాపీ సేవ్ చేయడానికి ముందు, solstone యాప్ ఇటీవలి లాగ్‌లు చదవండి. ఇక్కడి నుండి ఏమీ పంపబడదు.

linux.copy.recentlogs = ఇటీవలి లాగ్‌లు

linux.copy.restarting = రీస్టార్ట్ అవుతోంది…

linux.copy.restartneeded = రీస్టార్ట్ కావాలి

linux.copy.restartthesolstoneapp = solstone యాప్‌ను రీస్టార్ట్ చేయండి

linux.copy.resume = కొనసాగించండి

linux.copy.resuming = కొనసాగుతోంది…

linux.copy.s1 = స్క్రీన్‌షాట్‌లు

linux.copy.s10counting = సరిచూస్తోంది…

linux.copy.s10today = ఈ రోజు నుండి ప్రారంభించండి

linux.copy.s11 = ఎంత వెనక్కి మొదలు పెట్టాలో ఎంచుకుని, వాటిని తీసుకురండి.

linux.copy.s12 = మీ స్క్రీన్‌షాట్‌లను లెక్కించలేకపోయింది.

linux.copy.s14 = మీరు ఫోల్డర్ నుండి తొలగించినా, మీ జర్నల్‌లో ఇప్పటికే ఉన్న స్క్రీన్‌షాట్ అక్కడే ఉంటుంది.

linux.copy.s15 = మీరు పాజ్ చేసినా, లేదా దీన్ని ఆఫ్ చేసినా, మీ జర్నల్‌లో ఇప్పటికే ఉన్నది అలాగే ఉంటుంది.

linux.copy.s16computer = ఇది పాజ్ అయి ఉన్నప్పుడు మీరు తీసే స్క్రీన్‌షాట్‌లు లోపలికి రావు.

linux.copy.s17 = మీ జర్నల్‌లో ఇప్పటికే ఉన్న స్క్రీన్‌షాట్‌లు మళ్ళీ తీసుకురాబడవు.

linux.copy.s19 = మీ జర్నల్‌కు వెళ్లేందుకు వేచి ఉంది

linux.copy.s21 = journal అప్‌డేట్ కోసం వేచి ఉంది. మీ స్క్రీన్‌షాట్‌లు ఈ కంప్యూటర్‌లో వేచి ఉన్నాయి.

linux.copy.s22 = ఫోల్డర్

linux.copy.s23 = solstone యాప్ స్క్రీన్‌షాట్ అని గుర్తించగలిగింది మాత్రమే లోపలికి వస్తుంది.

linux.copy.s7 = ఈ రోజుకు ముందు నుండి

linux.copy.s9 = ఏవీ లోపలికి రాకముందే, స్క్రీన్‌షాట్‌లు ఎన్ని ఉన్నాయో మీరు చూస్తారు.

linux.copy.saveacopy = కాపీని సేవ్ చేయండి…

linux.copy.savesettings = సెట్టింగ్‌లు సేవ్ చేయండి

linux.copy.saving = సేవ్ అవుతోంది…

linux.copy.savingcopy = కాపీ సేవ్ అవుతోంది…

linux.copy.savingwritesexactlythepreviewabove = సేవ్ చేయడం పైన ఉన్న ప్రివ్యూ‌ను ఉన్నది ఉన్నట్లు రాస్తుంది.

linux.copy.screen = స్క్రీన్

linux.copy.screenandaudioarepauseduntilyouresumedesktop = మీరు కొనసాగించే వరకు స్క్రీన్ మరియు ఆడియో పాజ్ అయి ఉంటాయి. డెస్క్‌టాప్ కార్యకలాపం కొనసాగుతుంది.

linux.copy.screenandaudiostillpausedesktopactivitykeeps = స్క్రీన్ మరియు ఆడియో ఇంకా పాజ్ అవుతాయి. డెస్క్‌టాప్ కార్యకలాపం కొనసాగుతుంది.

linux.copy.screenaudiodesktopactivity = స్క్రీన్, ఆడియో మరియు డెస్క్‌టాప్ కార్యకలాపం

linux.copy.screenintakeiswaiting = స్క్రీన్ తీసుకోలు వేచి ఉంది

linux.copy.screenshotslookback76 = గత 30 రోజులు

linux.copy.screenshotslookback77 = గత 6 నెలలు

linux.copy.screenshotslookback78 = గత సంవత్సరం

linux.copy.screenshotslookback79 = అంతా

linux.copy.screenshotslookback80 = ఈ రోజుకు ముందు నుండి ఏమీ లేదు

linux.copy.seconds = సెకన్లు

linux.copy.segmentinterval = సెగ్మెంట్ విరామం

linux.copy.sentfoundnotesjournal = { $sent } / { $found } నోట్‌లు మీ జర్నల్‌లో

linux.copy.settings = సెట్టింగ్‌లు

linux.copy.settingsactivityonlyhelp = డెస్క్‌టాప్ కార్యకలాపం మాత్రమే, స్క్రీన్ మరియు ధ్వని ఆఫ్.

linux.copy.settingssaved = సెట్టింగ్‌లు సేవ్ అయ్యాయి.

linux.copy.solstoneappcantread = solstone యాప్ ఈ వాల్ట్ చదవలేకపోయింది. దాని ఫోల్డర్ ఉందో, అది ఉన్న డ్రైవ్ కనెక్ట్ అయి ఉందో సరిచూడండి. అది కనెక్ట్ కానంతవరకు మీ జర్నల్‌లో ఏమీ మారదు.

linux.copy.solstoneappcouldntsave = solstone యాప్ ఈ వాల్ట్ స్థానం సేవ్ చేయలేకపోయింది. ఈ కంప్యూటర్‌లో ఖాళీ చోటు ఉందో సరిచూడండి.

linux.copy.solstoneappdidntfind = solstone యాప్ ఈ కంప్యూటర్‌లో వాల్ట్ దొరకలేదు. బదులుగా దాని ఫోల్డర్ ఎంచుకోండి.

linux.copy.solstoneapptakesnotesone = solstone యాప్ ఈ కంప్యూటర్‌లోని ఒక వాల్ట్ నోట్‌లను స్వీకరిస్తుంది. అన్నీ మీ జర్నల్‌లోకి వెళ్తాయి, తర్వాత ఒక్కోటి మారినప్పుడు. మీ వాల్ట్ ఉన్నట్టే ఉంటుంది.

linux.copy.somenotescouldntgo = కొన్ని నోట్‌లు మీ జర్నల్‌లోకి వెళ్ళలేకపోయాయి. solstone యాప్ తర్వాత స్టార్ట్ అయినప్పుడు వాటిని మళ్ళీ ప్రయత్నిస్తుంది.

linux.copy.somethingansweredinsteadjournal = మీ జర్నల్ బదులు ఏదో సమాధానం ఇచ్చింది

linux.copy.somethingansweredthiscomputerat = { $address } వద్ద ఈ కంప్యూటర్‌కు ఏదో సమాధానం ఇచ్చింది, అది ఈ కంప్యూటర్ పెయిర్ అయిన journal కాదు. ఆ చిరునామాను ఇప్పుడు మరో పరికరం వాడుతూ ఉండవచ్చు, లేదా మీ జర్నల్, దాని గుర్తింపు రీసెట్ అయి ఉంటే. "ఏమి సమాధానం ఇచ్చింది" కింద ఉన్న మార్క్, మీ జర్నల్ ఇప్పుడు చూపే మార్క్‌తో సరిపోతే, ఈ కంప్యూటర్‌ను మళ్ళీ పెయిర్ చేయండి.

linux.copy.somethingansweredthiscomputerthrough = రిలే ద్వారా ఈ కంప్యూటర్‌కు ఏదో సమాధానం ఇచ్చింది, అది ఈ కంప్యూటర్ పెయిర్ అయిన journal కాదు. ఇది ప్రత్యక్ష కనెక్షన్ కంటే తక్కువ ఖచ్చితం.

linux.copy.starting = స్టార్ట్ అవుతోంది…

linux.copy.startpaused = పాజ్‌తో ప్రారంభించండి

linux.copy.startthesolstoneapp = solstone యాప్‌ను స్టార్ట్ చేయండి

linux.copy.statusunavailable = స్థితి అందుబాటులో లేదు

linux.copy.syncingjournal = మీ జర్నల్‌కు సింక్ అవుతోంది…

linux.copy.takestmuxsessionsallit = మీ { $literal1 } సెషన్‌లను స్వీకరిస్తుంది, అంతా మీ జర్నల్‌లోకి వెళ్తుంది. ఈ కంప్యూటర్‌లో { $literal1 } కావాలి. { $literal2 } ద్వారా, డెస్క్‌టాప్ లేకుండా కూడా పని చేస్తుంది.

linux.copy.terminal = టర్మినల్

linux.copy.thatdoesntmatch = అది సరిపోదు

linux.copy.thatfolderisntobsidian = ఆ ఫోల్డర్ { $literal1 } వాల్ట్ కాదు, కాబట్టి solstone యాప్ దాన్ని చేర్చలేదు. వాల్ట్ సొంత ఫోల్డర్‌ను ఎంచుకోండి.

linux.copy.thatfoldersnamecan = ఆ ఫోల్డర్ పేరు మీ జర్నల్‌లో వాల్ట్ పేరు కాదు. ఫోల్డర్ పేరు మార్చి మళ్ళీ ప్రయత్నించండి.

linux.copy.thatpairinglinkisntonethesolstoneappcanrea = ఆ పెయిరింగ్ లింక్ solstone యాప్ చదవగలిగేది కాదు. మీ జర్నల్ నుండి కొత్త పెయిరింగ్ లింక్ తెచ్చుకుని మళ్ళీ ప్రయత్నించండి.

linux.copy.thefolderisntonthiscomputer = { $folder } ఫోల్డర్ ఈ కంప్యూటర్‌లో ఇక లేదు.

linux.copy.thelogpreviewwaslimitedmorelogtextmaybeava = { $ws1 }[లాగ్ ప్రివ్యూ పరిమితం అయింది; మరింత లాగ్ వచనం అందుబాటులో ఉండవచ్చు]{ $ws1 }

linux.copy.thepairingchangedwhileyouwerecheckingitsmark = మీరు దాని మార్క్‌ను సరిచూస్తుండగా పెయిరింగ్ మారింది. నిర్ధారించే ముందు ప్రస్తుత మార్క్‌ను సరిచూడండి.

linux.copy.thepairingwindowclosedgetanewpairinglinkfro = పెయిరింగ్ విండో మూసుకుంది. మీ జర్నల్ నుండి కొత్త పెయిరింగ్ లింక్ తెచ్చుకుని, మళ్ళీ ప్రయత్నించండి.

linux.copy.thesetakeeffectthenexttimethesolstoneappsta = ఇవి solstone యాప్ తర్వాతి సారి స్టార్ట్ అయినప్పుడు అమలు అవుతాయి.

linux.copy.thesolstoneappcannowconnecttoyourjournal = solstone యాప్ ఇప్పుడు మీ జర్నల్‌కు కనెక్ట్ కాగలదు.

linux.copy.thesolstoneapphasntansweredyetcheckforades = solstone యాప్ ఇంకా సమాధానం ఇవ్వలేదు. డెస్క్‌టాప్ పంచుకునే డైలాగ్ ఉందో చూడండి, తర్వాత స్థితిని సరిచూడండి.

linux.copy.thesolstoneapprestartedwithyoursavedsettings = మీరు సేవ్ చేసిన సెట్టింగ్‌లతో solstone యాప్ రీస్టార్ట్ అయింది.

linux.copy.thewindowcouldntstartopenxfordetails = విండో స్టార్ట్ కాలేదు. వివరాల కోసం { $path } తెరవండి.

linux.copy.thewindowcouldntstarttryopeningitagain = విండో స్టార్ట్ కాలేదు. దాన్ని మళ్ళీ తెరవడం ప్రయత్నించండి.

linux.copy.thewindowneedsgtk46ornewercouldntloadits = విండోకు { $literal1 } 4.6 లేదా తర్వాతిది కావాలి. దాని డెస్క్‌టాప్ లైబ్రరీలను లోడ్ చేయలేకపోయింది. వివరాల కోసం { $path } తెరవండి.

linux.copy.thisisjournal = ఇది మీ జర్నల్

linux.copy.thismovessolstonetmuxinto = solstone for { $literal1 } ఈ యాప్‌లోకి తరలుతుంది, అది శాశ్వతంగా ఆగుతుంది. వేచి ఉన్నది ఈ యాప్ ద్వారా మీ జర్నల్‌లోకి వెళ్తుంది, వెళ్ళలేనిది ఈ కంప్యూటర్‌లోనే ఉంటుంది. ఈ యాప్ ఇంకా పెయిర్ కాకపోతే, solstone for { $literal1 } వాడే journalతో పెయిర్ అవుతుంది.

linux.copy.thisstartsthesolstoneappnowandatsigninopen = ఇది solstone యాప్‌ను ఇప్పుడు, మరియు సైన్-ఇన్ వద్ద, స్టార్ట్ చేస్తుంది. ఈ విండో తెరవడం వల్ల తీసుకోలు స్టార్ట్ కాదు.

linux.copy.toolargebring = తీసుకురావడానికి చాలా పెద్దది

linux.copy.tryagain = మళ్ళీ ప్రయత్నించండి

linux.copy.turningthisoffstopsbringing = దీన్ని ఆఫ్ చేయడం మార్పులు తీసుకురావడం ఆపుతుంది. మీ జర్నల్‌లో ఇప్పటికే ఉన్నది అక్కడే ఉంటుంది, వేచి ఉన్నది మీరు మళ్ళీ ఆన్ చేసే వరకు ఈ కంప్యూటర్‌లో ఉంటుంది.

linux.copy.turnoff = ఆఫ్ చేయండి

linux.copy.unavailable = అందుబాటులో లేదు

linux.copy.updateshow = ఆటోమేటిక్ అప్‌డేట్‌లు ఆన్‌లో ఉంటే, solstone ఇన్‌స్టాలర్ ఈ కంప్యూటర్‌లో రెండింటినీ సెటప్ చేసి ఉంటే, యాప్ మీ జర్నల్‌తో పాటు అప్‌డేట్ అవుతుంది. లేకపోతే, --upgrade తో solstone ఇన్‌స్టాలర్‌ను మళ్ళీ రన్ చేయండి.

linux.copy.updatesjournalversion = జర్నల్ వెర్షన్

linux.copy.usethisvault = ఈ వాల్ట్‌ను వాడండి

linux.copy.vault = వాల్ట్

linux.copy.vaultsthiscomputer = ఈ కంప్యూటర్‌లోని వాల్ట్‌లు

linux.copy.vaultwiththisnameis = ఈ పేరుతో వాల్ట్ మీ జర్నల్‌లో ఇప్పటికే ఉంది

linux.copy.versionvalue = యాప్ వెర్షన్: { $value }

linux.copy.viewdetails = వివరాలు చూడండి

linux.copy.waitingforthesolstoneapptoanswerifyourdeskt = solstone యాప్ సమాధానం కోసం వేచి ఉంది. మీ డెస్క్‌టాప్ ఏమి పంచుకోవాలో అడిగితే, అక్కడ ఎంపిక చేయండి.

linux.copy.waitingforyoutoconfirmyourjournalsmark = మీ జర్నల్ మార్క్‌ను మీరు నిర్ధారించేందుకు వేచి ఉంది

linux.copy.waitingjournalupdate = జర్నల్ అప్‌డేట్ కోసం వేచి ఉంది

linux.copy.waitingreachjournal = మీ జర్నల్‌కు చేరడానికి వేచి ఉంది

linux.copy.waitingtosync = సింక్ కోసం వేచి ఉంది

linux.copy.waitingvalue = సింక్ కోసం వేచి ఉంది: { $value }

linux.copy.webaddressesturnsondesktopaccessibility = వెబ్ చిరునామాలు (డెస్క్‌టాప్ యాక్సెసిబిలిటీ ఆన్ అవుతుంది)

linux.copy.whatanswered = ఏది సమాధానం ఇచ్చింది

linux.copy.windowtitles = విండో శీర్షికలు

linux.copy.windowtitlesleftoutofdesktopactivity = డెస్క్‌టాప్ కార్యకలాపం నుండి మినహాయించిన విండో శీర్షికలు

linux.copy.yesthisismyjournal = అవును, ఇది నా జర్నల్

linux.copy.your = మీ

linux.copy.yourcurrentpairingstaysinuseuntilyouconfirm = ఈ మార్క్‌ను మీరు నిర్ధారించే వరకు మీ ప్రస్తుత పెయిరింగ్ వాడుకలోనే ఉంటుంది.

linux.copy.yourjournalnotsetupyet = మీ జర్నల్, ఇంకా సెటప్ కాలేదు

linux.copy.yourjournalsmarkisconfirmedbutthesolstoneap = మీ జర్నల్ మార్క్ నిర్ధారించబడింది, కానీ solstone యాప్ రీస్టార్ట్ కాలేదు. మళ్ళీ ప్రయత్నించండి.

linux.copy.yourjournalsmarkisconfirmedbutthesolstoneap2 = మీ జర్నల్ మార్క్ నిర్ధారించబడింది, కానీ solstone యాప్‌కు శ్రద్ధ కావాలి. వివరాలు సరిచూసి, మళ్ళీ ప్రయత్నించండి.

linux.copy.yourjournalsmarkunavailablerightnow = మీ జర్నల్ మార్క్, ఇప్పుడు అందుబాటులో లేదు

linux.copy.yoursettingsaresavedbutthesolstoneappcouldn = మీ సెట్టింగ్‌లు సేవ్ అయ్యాయి, కానీ solstone యాప్ రీస్టార్ట్ కాలేదు. మళ్ళీ ప్రయత్నించండి.

linux.copy.yoursettingsaresavedrestartthesolstoneappto = మీ సెట్టింగ్‌లు సేవ్ అయ్యాయి. వాటిని ఇప్పుడే వాడటానికి solstone యాప్‌ను రీస్టార్ట్ చేయండి.

linux.copy.yoursettingschangedoutsidethiswindowreadthem = ఈ విండో బయట మీ సెట్టింగ్‌లు మారాయి. సేవ్ చేయడానికి ముందు వాటిని మళ్ళీ చదవండి.

linux.copy.yousaidthismarkdoesntmatchtheoneyourjourna2 = ఈ మార్క్ మీ జర్నల్ చూపేదితో సరిపోలేదని మీరు చెప్పారు. మీ ప్రస్తుత పెయిరింగ్ వాడుకలోనే ఉంటుంది. మీ జర్నల్ నుండి కొత్త పెయిరింగ్ లింక్ తీసుకోండి, లేదా ఇమెయిల్ పంపండి: { $literal1 }. మేము సహాయం చేస్తాము.

linux.desktop.clipboardcommandexitedunsuccessfully = క్లిప్‌బోర్డ్ కమాండ్ విజయవంతం కాలేదు

linux.desktop.couldnotopenyourjournalwaitforthesolstoneap = మీ జర్నల్‌ను తెరవలేకపోయింది. solstone యాప్ మళ్ళీ కనెక్ట్ అయే వరకు వేచి ఉండి, మళ్ళీ ప్రయత్నించండి.

linux.desktop.couldntcopyrunsolstonelinuxabouttoselectand = కాపీ చేయలేకపోయింది. మీ వెర్షన్‌లను ఎంచుకుని కాపీ చేయడానికి `{ $literal1 } about` రన్ చేయండి.

linux.desktop.couldntcopywithoutxinstallitorrunsolstonel = { $value1 } లేకుండా కాపీ చేయలేకపోయింది. దాన్ని ఇన్‌స్టాల్ చేయండి, లేదా మీ వెర్షన్‌లను ఎంచుకుని కాపీ చేయడానికి `{ $literal1 } about` రన్ చేయండి.

linux.desktop.observercommandmustberoutedbytherunloopowne = observer కమాండ్‌ను రన్-లూప్ యజమాని దారి మళ్ళించాలి

linux.doctor.cantreachyourdesktopxlogoutandbackintore = మీ డెస్క్‌టాప్‌ను చేరలేకపోయింది ({ $value1 }). దాన్ని రీస్టోర్ చేయడానికి లాగ్ అవుట్ చేసి మళ్ళీ లాగిన్ చేయండి.

linux.doctor.couldnotreachthesessionbustocheckthepaneli = ప్యానెల్ ఐకాన్‌ను సరిచూడటానికి సెషన్ బస్‌ను చేరలేకపోయింది

linux.doctor.couldnotreadloginsessionsx = లాగిన్ సెషన్‌లను చదవలేకపోయింది: { $error }

linux.doctor.couldnotstarttheprobex = ప్రోబ్‌ను స్టార్ట్ చేయలేకపోయింది: { $error }

linux.doctor.displaynotsetstartsolstonelinuxfromthex11gr = { $literal1 } పెట్టలేదు. { $literal3 } గ్రాఫికల్ సెషన్ నుండి { $literal2 } స్టార్ట్ చేయండి.

linux.doctor.doctor9checksxfailedxwarnings = { $ws1 }doctor: 9 సరిచూపులు, { $failures } విఫలమైనవి, { $warnings } హెచ్చరికలు

linux.doctor.followingsessionxwayland = అనుసరిస్తున్న సెషన్ { $session } (wayland)

linux.doctor.followingsessionxx11onx = అనుసరిస్తున్న సెషన్ { $session } (x11, { $display } పై)

linux.doctor.gnomeneedsanextensionrunsolstonelinuxpanelic = { $literal1 } కోసం ఎక్స్‌టెన్షన్ కావాలి. { $literal2 } రన్ చేయండి.

linux.doctor.gstreamerelementsxmissinginstallthepipewirea = { $literal1 } ఎలిమెంట్(లు) { $value1 } లేవు. { $literal2 } మరియు good/base ప్లగిన్ ప్యాకేజీలను ఇన్‌స్టాల్ చేయండి.

linux.doctor.gstreamerinitializationfailedxinstallgstreamer = { $literal1 } ప్రారంభం విఫలమైంది: { $error }. { $literal1 } 1.x ఇన్‌స్టాల్ చేయండి.

linux.doctor.label.desktopsession = డెస్క్‌టాప్ సెషన్

linux.doctor.label.fail =fail

linux.doctor.label.gstreamer = gstreamer

linux.doctor.label.na = n/a

linux.doctor.label.ok = ok

linux.doctor.label.panelicon = ప్యానెల్ ఐకాన్

linux.doctor.label.portal = xdg-desktop-portal

linux.doctor.label.pulse = pipewire (pulse)

linux.doctor.label.sessiontype = సెషన్ రకం

linux.doctor.label.synchealth = సింక్ స్థితి

linux.doctor.label.systemduser = systemd --user

linux.doctor.label.warn = హెచ్చరిక

linux.doctor.label.x11capture = x11 క్యాప్చర్

linux.doctor.nodesktopsessionofyoursonthis = ఈ కంప్యూటర్‌లో మీ డెస్క్‌టాప్ సెషన్ లేదు. స్క్రీన్ మరియు ఆడియోకు ఒకటి కావాలి.

linux.doctor.nodesktopsessionofyoursonthiscomputernothing = ఈ కంప్యూటర్‌లో మీ డెస్క్‌టాప్ సెషన్ లేదు. మీరు ఇక్కడ లాగిన్ చేసే వరకు స్క్రీన్ మరియు ఆడియో వేచి ఉంటాయి (మీరు రిమోట్‌గా లాగిన్ చేసే సెషన్ లెక్కలోకి రాదు).

linux.doctor.notapplicablenognomeshellonthesessionbus = వర్తించదు (సెషన్ బస్ పై { $literal1 } Shell లేదు)

linux.doctor.notapplicablenox11display = వర్తించదు ({ $literal1 } display లేదు)

linux.doctor.notapplicablewaylandsession = వర్తించదు (wayland సెషన్)

linux.doctor.notregisterednotneededonx11usingximagesrc = రిజిస్టర్ కాలేదు — { $literal1 } కోసం అవసరం లేదు ({ $literal2 } వాడుతోంది)

linux.doctor.okbrowserregistrationchromechromiumedgeandfir = ok{ $ws1 }బ్రౌజర్ రిజిస్ట్రేషన్{ $ws2 }chrome, chromium, edge మరియు firefox

linux.doctor.okfirefoxsnapallowedtostartthesolstoneapp = ok{ $ws1 }firefox (snap){ $ws2 }solstone యాప్‌ను స్టార్ట్ చేయడానికి అనుమతి ఉంది

linux.doctor.okfirefoxsnapasksyouoncetoletitstarttheso = ok{ $ws1 }firefox (snap){ $ws2 }solstone యాప్‌ను స్టార్ట్ చేయడానికి ఒకసారి అడుగుతుంది

linux.doctor.orgfreedesktopportaldesktopnotregisteredon = { $literal1 } సెషన్ బస్ పై రిజిస్టర్ కాలేదు. { $literal2 } ఇన్‌స్టాల్ చేసి స్టార్ట్ చేయండి.

linux.doctor.orgfreedesktopportaldesktopregisteredonsession = { $literal1 } సెషన్ బస్ పై రిజిస్టర్ అయి ఉంది

linux.doctor.pipewiresrcvp8encandwebmmuxavailable = pipewiresrc, vp8enc, మరియు webmmux అందుబాటులో ఉన్నాయి

linux.doctor.probepanickedinspectservicelogsandretry = ప్రోబ్ పానిక్ అయింది. సర్వీస్ లాగ్‌లు పరిశీలించి మళ్ళీ ప్రయత్నించండి.

linux.doctor.pulseaudiocompatibleserverreachable = { $literal1 } తో అనుకూలమైన సర్వర్ చేరగలదు

linux.doctor.pulseaudiocompatibleserverunreachablexstartpip = { $literal1 } తో అనుకూలమైన సర్వర్ చేరడం లేదు: { $error }. { $literal2 } Pulse లేదా { $literal1 } స్టార్ట్ చేయండి.

linux.doctor.screenaudioanddesktopactivityareoff = స్క్రీన్, ఆడియో మరియు డెస్క్‌టాప్ కార్యకలాపం ఆఫ్‌లో ఉన్నాయి

linux.doctor.sessionbusprobefailedxstartagraphicalsession = సెషన్ బస్ ప్రోబ్ విఫలమైంది: { $error }. గ్రాఫికల్ సెషన్ స్టార్ట్ చేసి, { $literal1 } ఇన్‌స్టాల్ అయి ఉందో సరిచూడండి.

linux.doctor.sessionbusunreachablexstartagraphicalsession = సెషన్ బస్ చేరడం లేదు: { $error }. గ్రాఫికల్ సెషన్ స్టార్ట్ చేసి, { $literal1 } ఇన్‌స్టాల్ అయి ఉందో సరిచూడండి.

linux.doctor.systemctlusernotreachableruninsideasystemdus = { $literal1 } చేరడం లేదు. systemd యూజర్ సెషన్ లోపల రన్ చేయండి.

linux.doctor.systemctlusernotreachablexruninsideasystemd = { $literal1 } చేరడం లేదు: { $error }. systemd యూజర్ సెషన్ లోపల రన్ చేయండి.

linux.doctor.thegnomeextensionisinstalledbutoffrunsolston = { $literal1 } ఎక్స్‌టెన్షన్ ఇన్‌స్టాల్ అయి ఉంది, కానీ ఆఫ్‌లో ఉంది. { $literal2 } రన్ చేయండి.

linux.doctor.thegnomeextensionisonbutnopaneliconyetlog = { $literal1 } ఎక్స్‌టెన్షన్ ఆన్‌లో ఉంది, కానీ ప్యానెల్ ఐకాన్ ఇంకా లేదు. అలాగే ఉంటే లాగ్ అవుట్ చేసి మళ్ళీ లాగిన్ చేయండి.

linux.doctor.thepaneliconextensionisinstalledlogoutandba = ప్యానెల్ ఐకాన్ ఎక్స్‌టెన్షన్ ఇన్‌స్టాల్ అయి ఉంది. పూర్తి చేయడానికి లాగ్ అవుట్ చేసి మళ్ళీ లాగిన్ చేయండి.

linux.doctor.thisdesktopcanshowthepanelicon = ఈ డెస్క్‌టాప్ ప్యానెల్ ఐకాన్‌ను చూపగలదు

linux.doctor.timedoutafter2schecktheuserdbussessionand = 2s తర్వాత టైమ్‌అవుట్ అయింది. యూజర్ D-Bus సెషన్‌ను సరిచూడండి, మరియు { $literal1 } కూడా.

linux.doctor.unrecognizedsessiontypexwaylandorx11required = గుర్తుపట్టని సెషన్ రకం '{ $value }'. { $literal1 } లేదా { $literal2 } కావాలి.

linux.doctor.warnbrowserregistrationmissingoroutofdatefor = హెచ్చరిక{ $ws1 }బ్రౌజర్ రిజిస్ట్రేషన్{ $ws2 }{ $value1 } కోసం లేదు, లేదా పాతబడింది. వాటిని మళ్ళీ రాయడానికి solstone యాప్‌ను స్టార్ట్ చేయండి.

linux.doctor.warnchromiumsnapcantreachthesolstoneappasn = హెచ్చరిక{ $ws1 }chromium (snap){ $ws2 }solstone యాప్‌ను చేరలేదు. chromium యొక్క snap దాన్ని స్టార్ట్ చేసే దారి లేదు. chrome, edge లేదా firefox వాడండి.

linux.doctor.warnfirefoxsnapasksyouoncetoletitstartthe = హెచ్చరిక{ $ws1 }firefox (snap){ $ws2 }solstone యాప్‌ను స్టార్ట్ చేయడానికి ఒకసారి అడుగుతుంది. మీరు ఏమి సమాధానం ఇచ్చారో doctor చదవలేకపోయింది.

linux.doctor.warnfirefoxsnapyoutoldyourdesktopnottoletf = హెచ్చరిక{ $ws1 }firefox (snap){ $ws2 }firefox solstone యాప్‌ను స్టార్ట్ చేయకూడదని మీరు మీ డెస్క్‌టాప్‌కు చెప్పారు, కాబట్టి దాని నుండి ఏమీ స్వీకరించబడదు. మళ్ళీ అడగాలంటే, ఇది రన్ చేయండి:{ $ws3 }gdbus { $value1 } --method { $literal4 } { $PERMISSION-TABLE } { $value2 } { $SNAP-FIREFOX }

linux.doctor.warnxcantreachthesolstoneappfrominsideafl = హెచ్చరిక{ $ws1 }{ $id }{ $ws1 }flatpak లోపల నుండి solstone యాప్‌ను చేరలేదు. flatpak బయట ఇన్‌స్టాల్ చేసిన chrome, edge లేదా firefox వాడండి.

linux.doctor.wayland = wayland

linux.doctor.x11randrandximagesrcavailable = { $literal1 } RandR మరియు { $literal2 } అందుబాటులో ఉన్నాయి

linux.doctor.x11randrunavailablexcheckdisplayandxserverr = { $literal1 } RandR అందుబాటులో లేదు: { $error }. { $literal2 } మరియు X సర్వర్ RandR మద్దతు సరిచూడండి.

linux.doctor.x11usingximagesrccapture = x11 ({ $literal1 } క్యాప్చర్ వాడుతోంది)

linux.doctor.xcheckthatsystemdlogindisrunning = { $value1 }. { $literal1 } నడుస్తోందో సరిచూడండి.

linux.doctor.xdgsessiontypenotsetwaylandor = { $literal1 } పెట్టలేదు. { $literal2 } లేదా { $literal3 } కావాలి.

linux.doctor.ximagesrcmissinginstallthegstreamergoodplugins = { $literal1 } లేదు. { $literal2 } good ప్లగిన్ ప్యాకేజీని ఇన్‌స్టాల్ చేయండి.

linux.doctor.yourdesktopsessionisnttheoneinfrontofthes = మీ డెస్క్‌టాప్ సెషన్ స్క్రీన్ ముందు ఉన్నది కాదు. అది ముందు వచ్చే వరకు స్క్రీన్ మరియు ఆడియో వేచి ఉంటాయి.

linux.health.idleconfirmingwithyourjournal = పని లేకుండా, మీ జర్నల్‌తో నిర్ధారిస్తోంది

linux.health.idleconnected = పని లేకుండా, కనెక్ట్ అయి ఉంది

linux.health.idleconnecting = పని లేకుండా, కనెక్ట్ అవుతోంది

linux.health.idleconnectionunavailableheldonthisdevice = పని లేకుండా, కనెక్షన్ అందుబాటులో లేదు (ఈ పరికరం‌లో నిలిచి ఉంది)

linux.health.idleconnectionunavailableheldonthisdevice2 = పని లేకుండా, కనెక్షన్ అందుబాటులో లేదు, ఈ పరికరం‌లో నిలిచి ఉంది

linux.health.idlenostatus = పని లేకుండా, స్థితి లేదు

linux.health.idleofflineheldonthisdevice = పని లేకుండా, ఆఫ్‌లైన్ (ఈ పరికరం‌లో నిలిచి ఉంది)

linux.health.idleofflineheldonthisdevice2 = పని లేకుండా, ఆఫ్‌లైన్, ఈ పరికరం‌లో నిలిచి ఉంది

linux.health.idlepairagain = పని లేకుండా, మళ్ళీ పెయిర్ చేయండి

linux.health.idlepairingnotsaved = పని లేకుండా, పెయిరింగ్ సేవ్ కాలేదు

linux.health.idlepairingrequired = పని లేకుండా, పెయిరింగ్ కావాలి

linux.health.idlepairingunsafe = పని లేకుండా, పెయిరింగ్ సురక్షితం కాదు

linux.health.idlesyncing = పని లేకుండా, సింక్ అవుతోంది

linux.health.idlesyncuptodate = పని లేకుండా, సింక్ తాజాగా ఉంది

linux.health.idleupdaterequired = పని లేకుండా, అప్‌డేట్ కావాలి

linux.health.journalmark = మీ జర్నల్:{ $ws1 }{ $paired }

linux.health.markclaim = { $spoken }{ $ws1 }({ $claim })

linux.health.markclaimed = క్లెయిమ్ చేయబడింది, ధృవీకరించబడలేదు

linux.health.n0pending = 0 వేచి ఉంది

linux.health.noidentitypresented = గుర్తింపు చూపించలేదు

linux.health.onconfirmingwithyourjournal = ఆన్, మీ జర్నల్‌తో నిర్ధారిస్తోంది

linux.health.onconnected = ఆన్, కనెక్ట్ అయి ఉంది

linux.health.onconnecting = ఆన్, కనెక్ట్ అవుతోంది

linux.health.onconnectionunavailableheldonthisdevice = ఆన్, కనెక్షన్ అందుబాటులో లేదు (ఈ పరికరం‌లో నిలిచి ఉంది)

linux.health.onconnectionunavailableheldonthisdevice2 = ఆన్, కనెక్షన్ అందుబాటులో లేదు, ఈ పరికరం‌లో నిలిచి ఉంది

linux.health.onnostatus = ఆన్, స్థితి లేదు

linux.health.onofflineheldonthisdevice = ఆన్, ఆఫ్‌లైన్ (ఈ పరికరం‌లో నిలిచి ఉంది)

linux.health.onofflineheldonthisdevice2 = ఆన్, ఆఫ్‌లైన్, ఈ పరికరం‌లో నిలిచి ఉంది

linux.health.onpairagain = ఆన్, మళ్ళీ పెయిర్ చేయండి

linux.health.onpairingnotsaved = ఆన్, పెయిరింగ్ సేవ్ కాలేదు

linux.health.onpairingrequired = ఆన్, పెయిరింగ్ కావాలి

linux.health.onpairingunsafe = ఆన్, పెయిరింగ్ సురక్షితం కాదు

linux.health.onsyncing = ఆన్, సింక్ అవుతోంది

linux.health.onsyncuptodate = ఆన్, సింక్ తాజాగా ఉంది

linux.health.onupdaterequired = ఆన్, అప్‌డేట్ కావాలి

linux.health.pendingunconfirmed = వేచి ఉంది, నిర్ధారించబడలేదు

linux.health.sighting = { $header }:{ $ws1 }ఏమి సమాధానం ఇచ్చింది:{ $ws2 }{ $answer }{ $ws3 }{ $paired }{ $footer }

linux.health.syncconfirmingwithyourjournal = సింక్: మీ జర్నల్‌తో నిర్ధారిస్తోంది

linux.health.syncconnecteduptodate = సింక్: కనెక్ట్ అయి ఉంది; తాజాగా ఉంది

linux.health.syncconnecting = సింక్: కనెక్ట్ అవుతోంది

linux.health.syncconnectingwaitwhilethisdeviceconnectstoy = సింక్: కనెక్ట్ అవుతోంది. ఈ పరికరం మీ జర్నల్‌కు కనెక్ట్ అయే వరకు వేచి ఉండండి.

linux.health.syncconnectionunavailableheldonthisdevice = సింక్: కనెక్షన్ అందుబాటులో లేదు. ఈ పరికరం‌లో నిలిచి ఉంది.

linux.health.syncconnectionunavailableheldonthisdevicerest = సింక్: కనెక్షన్ అందుబాటులో లేదు. ఈ పరికరం‌లో నిలిచి ఉంది. solstone యాప్‌ను రీస్టార్ట్ చేయండి. ఇది కొనసాగితే, ఈ పరికరాన్ని మళ్ళీ పెయిర్ చేయండి.

linux.health.syncconnectionunavailablerestartthesolstoneapp = సింక్: కనెక్షన్ అందుబాటులో లేదు. solstone యాప్‌ను రీస్టార్ట్ చేయండి. ఇది కొనసాగితే, ఈ పరికరాన్ని మళ్ళీ పెయిర్ చేయండి.

linux.health.synchealthconnectingwaitwhilethisdeviceconnec = సింక్ స్థితి: కనెక్ట్ అవుతోంది. ఈ పరికరం మీ జర్నల్‌కు కనెక్ట్ అయే వరకు వేచి ఉండండి.

linux.health.synchealthconnectionunavailablerestartthesolst = సింక్ స్థితి: కనెక్షన్ అందుబాటులో లేదు. solstone యాప్‌ను రీస్టార్ట్ చేయండి. ఇది కొనసాగితే, ఈ పరికరాన్ని మళ్ళీ పెయిర్ చేయండి.

linux.health.synchealthofflineheldonthisdevicewillretry = సింక్ స్థితి: ఆఫ్‌లైన్. ఈ పరికరం‌లో నిలిచి ఉంది. మళ్ళీ ప్రయత్నిస్తుంది.

linux.health.synchealthpairingnotsavedfixthisdevicespair = సింక్ స్థితి: పెయిరింగ్ సేవ్ కాలేదు. ఈ పరికరం పెయిరింగ్ అనుమతులు సరిచేసి, solstone యాప్‌ను రీస్టార్ట్ చేయండి.

linux.health.synchealthpairingunsaferepairthisdevicespair = సింక్ స్థితి: పెయిరింగ్ సురక్షితం కాదు. ఈ పరికరం పెయిరింగ్‌ను బాగుచేసి solstone యాప్‌ను రీస్టార్ట్ చేయండి.

linux.health.synchealthpairthisdevicewithyourjournal = సింక్ స్థితి: ఈ పరికరాన్ని మీ జర్నల్‌తో పెయిర్ చేయండి

linux.health.synchealthpairthisdevicewithyourjournalagain = సింక్ స్థితి: ఈ పరికరాన్ని మీ జర్నల్‌తో మళ్ళీ పెయిర్ చేయండి

linux.health.synchealthupdaterequiredupdatethesolstoneapp = సింక్ స్థితి: అప్‌డేట్ కావాలి. solstone యాప్‌ను అప్‌డేట్ చేయండి.

linux.health.synchealthwaitwhilethisdeviceconfirmswithyou = సింక్ స్థితి: ఈ పరికరం మీ జర్నల్‌తో నిర్ధారించే వరకు వేచి ఉండండి.

linux.health.synchealthyourjournalisreceivingnotconfirmed = సింక్ స్థితి: సింక్ అవుతోంది. మీ జర్నల్ ఇంకా నిర్ధారించలేదు.

linux.health.syncingfallback = సింక్ అవుతోంది...

linux.health.syncnostatus = సింక్: స్థితి లేదు

linux.health.syncnostatusfromthesolstoneapprightnow = సింక్: ఇప్పుడు solstone యాప్ నుండి స్థితి లేదు

linux.health.syncnostatusthesolstoneappisnotrunningorha = సింక్: స్థితి లేదు. solstone యాప్ నడవడం లేదు, లేదా ఇంకా స్థితి లేదు.

linux.health.syncofflineheldonthisdevicewillretry = సింక్: ఆఫ్‌లైన్. ఈ పరికరం‌లో నిలిచి ఉంది. మళ్ళీ ప్రయత్నిస్తుంది.

linux.health.syncofflinewillretry = సింక్: ఆఫ్‌లైన్. మళ్ళీ ప్రయత్నిస్తుంది.

linux.health.syncpairagain = సింక్: మళ్ళీ పెయిర్ చేయండి

linux.health.syncpairagainpairthisdevicewithyourjournala = సింక్: మళ్ళీ పెయిర్ చేయండి. ఈ పరికరాన్ని మీ జర్నల్‌తో మళ్ళీ పెయిర్ చేయండి.

linux.health.syncpairingnotsaved = సింక్: పెయిరింగ్ సేవ్ కాలేదు

linux.health.syncpairingnotsavedfixthisdevicespairingper = సింక్: పెయిరింగ్ సేవ్ కాలేదు. ఈ పరికరం పెయిరింగ్ అనుమతులు సరిచేసి, solstone యాప్‌ను రీస్టార్ట్ చేయండి.

linux.health.syncpairingrequired = సింక్: పెయిరింగ్ కావాలి

linux.health.syncpairingrequiredpairthisdevicewithyourjou = సింక్: పెయిరింగ్ కావాలి. ఈ పరికరాన్ని మీ జర్నల్‌తో పెయిర్ చేయండి.

linux.health.syncpairingunsafe = సింక్: పెయిరింగ్ సురక్షితం కాదు

linux.health.syncpairingunsaferepairthisdevicespairingand = సింక్: పెయిరింగ్ సురక్షితం కాదు. ఈ పరికరం పెయిరింగ్‌ను బాగుచేసి solstone యాప్‌ను రీస్టార్ట్ చేయండి.

linux.health.syncpairthisdevicewithyourjournal = సింక్: ఈ పరికరాన్ని మీ జర్నల్‌తో పెయిర్ చేయండి

linux.health.syncpairthisdevicewithyourjournalagain = సింక్: ఈ పరికరాన్ని మీ జర్నల్‌తో మళ్ళీ పెయిర్ చేయండి

linux.health.syncprogress = సింక్: { $progress }

linux.health.syncsyncingyourjournalisreceivingnotconfirmed = సింక్: సింక్ అవుతోంది; మీ జర్నల్ ఇంకా నిర్ధారించలేదు

linux.health.syncupdaterequiredupdatethesolstoneapp = సింక్: అప్‌డేట్ కావాలి; solstone యాప్‌ను అప్‌డేట్ చేయండి

linux.health.syncupdatethesolstoneapp = సింక్: solstone యాప్‌ను అప్‌డేట్ చేయండి

linux.health.syncuptodate = సింక్: తాజాగా ఉంది

linux.health.syncwaitwhilethisdeviceconfirmswithyourjourn = సింక్: ఈ పరికరం మీ జర్నల్‌తో నిర్ధారించే వరకు వేచి ఉండండి

linux.health.syncwaitwhilethisdeviceconnectstoyourjournal = సింక్: ఈ పరికరం మీ జర్నల్‌కు కనెక్ట్ అయే వరకు వేచి ఉండండి

linux.health.thesolstoneappisnotrunningorhasnostatusyet = solstone యాప్ నడవడం లేదు, లేదా ఇంకా స్థితి లేదు

linux.health.unknownjournalseenatx = { $address } వద్ద తెలియని జర్నల్ కనబడింది

linux.health.unknownjournalseenthroughtherelay = రిలే ద్వారా తెలియని జర్నల్ కనిపించింది

linux.health.uptodateat = సింక్ స్థితి: { $timestamp } వద్ద తాజాగా ఉంది

linux.health.waitingforyoutoconfirmyourjournalsmarknothi = మీ జర్నల్ మార్క్‌ను మీరు నిర్ధారించేందుకు వేచి ఉంది. వేచి ఉన్నది ఏదీ, మీరు నిర్ధారించే వరకు, మీ జర్నల్‌లోకి వెళ్ళదు.{ $ws1 }మీరు సిద్ధంగా ఉన్నప్పుడు ఇది రన్ చేయండి: { $literal2 }

linux.import.allhistory = మీ జర్నల్‌లో ఇంపోర్ట్ చరిత్ర అంతా

linux.import.attention = శ్రద్ధ కావాలి

linux.import.cannotconvert = ఈ పరికరం ఈ ఫోటోను కన్వర్ట్ చేయలేకపోయింది

linux.import.delivered = మీ జర్నల్‌లో.

linux.import.expired = గడువు అయింది

linux.import.explanation = మీరు ఇక్కడ ఎంచుకునే లేదా ఈ విండోపై డ్రాప్ చేసే ఏ ఫైల్‌నైనా చేరుస్తుంది. అది ఏమిటో మీ జర్నల్ తేల్చుతుంది.

linux.import.explanationnodrop = ఇక్కడ మీరు ఎంచుకునే ఏ ఫైల్‌నైనా చేరుస్తుంది. అది ఏమిటో మీ జర్నల్ తేల్చుకుంటుంది.

linux.import.filekind = మీ జర్నల్ ఈ రకం ఫైల్‌ను స్వీకరించలేకపోతోంది

linux.import.filemissing = ఈ పరికరంలోని కాపీ లేదు

linux.import.filenotread = దీన్ని చదవలేకపోయింది. మళ్ళీ చేర్చండి

linux.import.filetoolarge = ఇది మీ జర్నల్ స్వీకరించే దానికంటే పెద్దది

linux.import.held = ఇప్పుడు మీ జర్నల్‌కు వెళ్లడం లేదు. ప్రతి ఒక్కటీ ఎందుకో చెబుతుంది.

linux.import.inyourjournalrel = మీ జర్నల్‌లో · { $when }

linux.import.journalrefusal = మీ జర్నల్ దీన్ని అంగీకరించలేకపోయింది.

linux.import.moreimports = మరిన్ని ఇంపోర్ట్‌లు

linux.import.moreimportssub = మీ జర్నల్‌లో గైడ్‌లు, ఆప్షన్‌లు, ఇంపోర్ట్ చరిత్ర

linux.import.namedrefusal = { $name }: { $reason }

linux.import.nfiles = { $count } ఫైల్‌లు

linux.import.noroom = ఈ పరికరంలో చోటు సరిపోదు

linux.import.nothingwassaved = ఈ పరికరంలో ఏమీ సేవ్ కాలేదు.

linux.import.nothingyet = ఇంకా ఏమీ లేదు.

linux.import.nreadyinjournal = మీ జర్నల్‌లో { $count } సిద్ధంగా ఉన్నాయి

linux.import.nwaiting = { $count } వేచి ఉన్నాయి

linux.import.pairagain = మీ జర్నల్ ఈ పరికరాన్ని అంగీకరించడం ఆపింది. మీరు మళ్ళీ పెయిరింగ్ చేసే వరకు ఇంపోర్ట్‌లు ఇక్కడ వేచి ఉంటాయి.

linux.import.readytoreview = సమీక్షించడానికి సిద్ధంగా ఉంది

linux.import.readytoreviewcount = { $count } సమీక్షించడానికి సిద్ధంగా ఉన్నాయి

linux.import.recentrow.a11y = { $name }, మీ జర్నల్‌లో, { $when }. మీ జర్నల్‌లో తెరవండి.

linux.import.remove.a11y = { $name } తీసివేయండి

linux.import.removeerror = దీన్ని తీసివేయలేకపోయింది. మళ్ళీ ప్రయత్నించండి.

linux.import.removereadytoreview = ఇది ఈ పరికరంలోని కాపీని తీసివేస్తుంది. మీ జర్నల్‌లో వేచి ఉన్న కాపీ, మీరు దాన్ని వదిలే వరకు లేదా అది గడువు అయే వరకు, అక్కడే ఉంటుంది.

linux.import.reviewdetail = మీ జర్నల్‌లో సేవ్ అయింది. ప్రారంభించడానికి ప్రివ్యూ చూసి నిర్ధారించండి.

linux.import.reviewdetailhour = మీ జర్నల్‌లో సేవ్ అయింది. వేచి ఉన్న కాపీని మీ జర్నల్ తీసివేసే ముందు ఒక గంటలోపు సమీక్షించండి.

linux.import.reviewexpireddetail = మీ జర్నల్‌లో వేచి ఉన్న కాపీ గడువు అయింది. ఈ పరికరం ఫైల్‌ను ఇంకా ఉంచుకుంటోంది.

linux.import.reviewinjournal = మీ జర్నల్‌లో సమీక్షించండి

linux.import.reviewinjournal.a11y = మీ జర్నల్‌లో { $name } సమీక్షించండి

linux.import.saved = ఈ పరికరం‌లో సేవ్ అయింది: { $count }

linux.import.saving = ఈ పరికరం‌లో సేవ్ అవుతోంది: { $total }లో { $saved }

linux.import.sendagain = సమీక్షకు మళ్ళీ పంపండి

linux.import.sendagain.a11y = సమీక్షకు { $name } మళ్ళీ పంపండి

linux.import.sending = పంపిస్తోంది

linux.import.sendingellipsis = మీ జర్నల్‌కు పంపిస్తోంది…

linux.import.sendingtojournal = మీ జర్నల్‌కు పంపుతోంది.

linux.import.statusneedsyou = ఇంపోర్ట్‌లకు మీరు కావాలి

linux.import.statusneedsyousub = ఎందుకో చూడటానికి ఇంపోర్ట్ తెరవండి.

linux.import.statusreadytoreview = { $count ->
    [one] { $display } ఇంపోర్ట్ సమీక్షించడానికి సిద్ధంగా ఉంది
   *[other] { $display } ఇంపోర్ట్‌లు సమీక్షించడానికి సిద్ధంగా ఉన్నాయి
}

linux.import.statusreadytoreviewsub = సమీక్షించి ప్రారంభించడానికి ఇంపోర్ట్ తెరవండి.

linux.import.statusrow.a11y = ఇంపోర్ట్, { $detail }.

linux.import.steperror = ఈ అడుగు పూర్తి కాలేకపోయింది. మళ్ళీ ప్రయత్నించండి.

linux.import.storeerror = ఈ పరికరంలోని ఇంపోర్ట్‌లను చదవలేకపోయింది. ఏమీ తీసివేయబడలేదు. మళ్ళీ ప్రయత్నించడానికి solstone యాప్‌ను రీస్టార్ట్ చేయండి.

linux.import.tile.a11y = ఇంపోర్ట్, { $state }. { $detail }.

linux.import.trynow.a11y = { $name } ఇప్పుడు ప్రయత్నించండి

linux.import.waitforjournal = మీరు ఒక జర్నల్‌ను కనెక్ట్ చేసే వరకు ఇంపోర్ట్‌లు ఈ పరికరంలో వేచి ఉంటాయి.

linux.import.waitformark = మీరు మీ జర్నల్ మార్క్‌ను నిర్ధారించే వరకు ఇంపోర్ట్‌లు వేచి ఉంటాయి.

linux.import.waitforupdate = మీ జర్నల్ అప్‌డేట్ అయే వరకు ఇంపోర్ట్‌లు వేచి ఉంటాయి.

linux.import.waiting = వేచి ఉంది

linux.import.waitingdetail = మీ జర్నల్‌కు వెళ్లేందుకు వేచి ఉంది

linux.import.waitingforjournal = మీ జర్నల్‌లోకి వెళ్ళడానికి వేచి ఉంది.

linux.import.waitingforjournalshort = మీ జర్నల్ కోసం వేచి ఉంది

linux.import.waitingforyourreview = మీ సమీక్ష కోసం వేచి ఉంది

linux.import.waitingreconnect = మీ జర్నల్ మళ్ళీ కనెక్ట్ అవ్వడానికి వేచి ఉంది.

linux.journalmark.journal = journal: { $value1 }·{ $value2 }

linux.journalmark.journaljournal = journal: మీ జర్నల్

linux.journalmark.journalnotpaired = journal: పెయిర్ కాలేదు

linux.journalmark.journalsmarkunavailableright = మీ జర్నల్ మార్క్: ఇప్పుడు అందుబాటులో లేదు

linux.journalmark.markwordsyougavedon = మీరు ఇచ్చిన మార్క్ పదాలు జర్నల్ మార్క్‌తో సరిపోలేదు, కాబట్టి ఈ కంప్యూటర్ పెయిర్ కాలేదు. పదాలు సరిచూసి, మీ జర్నల్ నుండి కొత్త పెయిరింగ్ లింక్ తీసుకుని మళ్ళీ పెయిర్ చేయండి.

linux.journalmark.markwordsyougavedon2 = మీరు ఇచ్చిన మార్క్ పదాలు జర్నల్ మార్క్‌తో సరిపోలేదు.

linux.journalmark.notchangedcurrentpairingstays = మారలేదు. మీ ప్రస్తుత పెయిరింగ్ అలాగే వాడుకలో ఉంటుంది. ఈ లింక్ ద్వారా, ఈ కంప్యూటర్ ఉంచినది ఏదీ ఆ journal కు వెళ్ళలేదు.

linux.journalmark.notpairedgetfreshpairing = పెయిర్ కాలేదు. మీ జర్నల్ నుండి కొత్త పెయిరింగ్ లింక్ తీసుకుని ఈ కంప్యూటర్‌ను పెయిర్ చేయండి.

linux.journalmark.pairingcancelledcurrentpairingstays = పెయిరింగ్ రద్దు అయింది. మీ ప్రస్తుత పెయిరింగ్ అలాగే వాడుకలో ఉంటుంది.

linux.journalmark.pairingcancelledgetfreshpairing = పెయిరింగ్ రద్దు అయింది. మీరు సిద్ధంగా ఉన్నప్పుడు మీ జర్నల్ నుండి కొత్త పెయిరింగ్ లింక్ తీసుకుని మళ్ళీ పెయిర్ చేయండి.

linux.journalmark.pastepairinglinkfromjournal = మీ జర్నల్ నుండి పెయిరింగ్ లింక్‌ను పేస్ట్ చేసి, ఎంటర్ నొక్కండి:{ $ws1 }

linux.journalmark.setupfailedpairinglinkwas = సెటప్ సాధ్యం కాలేదు: పెయిరింగ్ లింక్ చెల్లలేదు.

linux.journalmark.yousaidthismarkdoesn = ఈ మార్క్ మీ జర్నల్ చూపించేదానితో సరిపోలేదని మీరు చెప్పారు, కాబట్టి ఈ కంప్యూటర్ పెయిర్ కాలేదు. మీరు తప్పు లింక్ పేస్ట్ చేసి ఉండవచ్చు, లేదా ఏదో సరిగా లేదు. మీ జర్నల్ నుండి కొత్త పెయిరింగ్ లింక్ తీసుకుని మళ్ళీ పెయిర్ చేయండి, లేదా ఇమెయిల్ పంపండి: { $literal1 }. మేము సహాయం చేస్తాము.

linux.metadata.autostartcomment = మీరు పంచుకున్నదాన్ని solstone యాప్ స్వీకరిస్తుంది, అదంతా మీ జర్నల్‌లోకి వెళ్తుంది

linux.metadata.autostartname = solstone యాప్

linux.metadata.gnomedescription = { $command } కోసం డెస్క్‌టాప్ కార్యకలాప సహాయకం

linux.metadata.gnomename = solstone కార్యకలాప సహాయకం

linux.metadata.kwindescription = KWin కార్యకలాప సహాయకం, { $command } కోసం

linux.metadata.kwinname = solstone కార్యకలాప సహాయకం

linux.metadata.name = solstone

linux.metadata.packagedescription = linux కోసం solstone యాప్. మీరు పంచుకున్న స్క్రీన్‌ను మరియు ఆడియోను అది స్వీకరిస్తుంది, అదంతా మీ జర్నల్‌లోకి వెళ్తుంది.

linux.metadata.packagesummary = linux కోసం solstone యాప్: మీరు పంచుకున్నది మీ జర్నల్‌లోకి వెళ్తుంది

linux.metadata.service = linux కోసం solstone యాప్

linux.notification.copied = కాపీ అయింది

linux.notification.screenelementsmissing = స్క్రీన్ తీసుకోలు స్టార్ట్ కాలేదు: { $literal1 } లో { $elements } లేవు. ప్యాకేజీ మేనేజర్‌తో { $literal1 } { $literal2 }, good మరియు base ప్లగిన్‌లు ఇన్‌స్టాల్ చేసి, స్క్రీన్ పేజీలో మళ్ళీ ప్రయత్నించండి ఎంచుకోండి.

linux.notification.screenelementsmissingx11 = స్క్రీన్ తీసుకోలు స్టార్ట్ కాలేదు: { $literal1 } లో { $elements } లేవు. ప్యాకేజీ మేనేజర్‌తో { $literal1 } good మరియు base ప్లగిన్‌లు ఇన్‌స్టాల్ చేసి, స్క్రీన్ పేజీలో మళ్ళీ ప్రయత్నించండి ఎంచుకోండి.

linux.pair.cancel = రద్దు చేయండి

linux.pair.continue = కొనసాగించండి

linux.pair.onemorestepcheckyourjournalsmarkyourjournal = ఇంకో అడుగు: మీ జర్నల్ మార్క్‌ను సరిచూడండి.{ $ws1 }{ $ws2 }మీ జర్నల్ మార్క్: { $spoken }{ $ws1 }{ $ws1 }మీ జర్నల్ తన నెట్‌వర్క్ యాప్‌లో ఇదే మార్క్‌ను చూపిస్తుంది. అది సరిగ్గా సరిపోవాలి.{ $ws1 }ఇది మీ జర్నల్‌తో సరిపోతుందా? { $literal3 } లేదా { $literal4 } టైప్ చేయండి:{ $ws1 }

linux.pair.onemorestepcheckyourjournalsmarkyourjournal2 = ఇంకో అడుగు: మీ జర్నల్ మార్క్‌ను సరిచూడండి.{ $ws1 }{ $ws2 }మీ జర్నల్ మార్క్: ఇప్పుడు అందుబాటులో లేదు{ $ws1 }{ $ws1 }ధృవీకరించలేకపోయింది.{ $ws1 }ఈ కంప్యూటర్ మీ జర్నల్ మార్క్‌ను తేల్చలేకపోయింది, కాబట్టి పోల్చడానికి ఏమీ లేదు. లింక్ మీ జర్నల్ నుండి వచ్చిందని మీకు ఖచ్చితంగా తెలిస్తేనే { $literal3 }.{ $ws1 }అయినా పెయిర్ చేయడానికి { $literal3 } టైప్ చేయండి, లేదా ఆపడానికి { $literal4 }:{ $ws1 }

linux.pair.pairingcancellednothingthiscomputerhaskeptwen = పెయిరింగ్ రద్దు అయింది. ఈ లింక్ ద్వారా, ఈ కంప్యూటర్ ఉంచినది ఏదీ ఆ journal కు వెళ్ళలేదు. మీరు సిద్ధంగా ఉన్నప్పుడు మీ జర్నల్ నుండి కొత్త పెయిరింగ్ లింక్ తీసుకుని సెటప్ మళ్ళీ రన్ చేయండి.

linux.pair.themarkwordsyougavedontmatchthejournalsma = మీరు ఇచ్చిన మార్క్ పదాలు జర్నల్ మార్క్‌తో సరిపోలేదు, కాబట్టి ఈ కంప్యూటర్ పెయిర్ కాలేదు, మరియు ఈ లింక్ ద్వారా అది ఉంచినది ఏదీ ఆ journal కు వెళ్ళలేదు. పదాలు సరిచూసి, మీ జర్నల్ నుండి కొత్త పెయిరింగ్ లింక్ తీసుకుని సెటప్ మళ్ళీ రన్ చేయండి.

linux.pair.thiscomputercouldntworkoutthejournalsmarks = ఈ కంప్యూటర్ జర్నల్ మార్క్‌ను తేల్చలేకపోయింది, కాబట్టి మీరు ఇచ్చిన పదాలు సరిపోలవు. ఈ కంప్యూటర్ పెయిర్ కాలేదు, మరియు ఈ లింక్ ద్వారా అది ఉంచినది ఏదీ ఆ journal కు వెళ్ళలేదు. మీ జర్నల్ నుండి కొత్త పెయిరింగ్ లింక్ తీసుకుని, మీరే నిర్ణయించడానికి టర్మినల్ లో సెటప్ రన్ చేయండి.

linux.pair.thiscomputercouldntworkoutthejournalsmarks2 = ఈ కంప్యూటర్ జర్నల్ మార్క్‌ను తేల్చలేకపోయింది, కాబట్టి మీరు ఇచ్చిన పదాలు సరిపోలవు. మీ జర్నల్ మార్క్‌ను మీరు నిర్ధారించేందుకు వేచి ఉంది. వేచి ఉన్నది ఏదీ, మీరు నిర్ధారించే వరకు, మీ జర్నల్‌లోకి వెళ్ళదు. మీరే నిర్ణయించడానికి టర్మినల్ లో { $literal1 } రన్ చేయండి.

linux.pair.whenyourereadyrunsolstonelinuxconfirm = మీరు సిద్ధంగా ఉన్నప్పుడు ఇది రన్ చేయండి: { $literal1 }

linux.pair.yousaidthismarkdoesntmatchtheoneyourjourna = ఈ మార్క్ మీ జర్నల్ చూపించేదానితో సరిపోలేదని మీరు చెప్పారు, కాబట్టి ఈ కంప్యూటర్ పెయిర్ కాలేదు, మరియు ఈ లింక్ ద్వారా అది ఉంచినది ఏదీ ఆ journal కు వెళ్ళలేదు. మీరు తప్పు లింక్ పేస్ట్ చేసి ఉండవచ్చు, లేదా ఏదో సరిగా లేదు. మీ జర్నల్ నుండి కొత్త పెయిరింగ్ లింక్ తీసుకుని సెటప్ మళ్ళీ రన్ చేయండి, లేదా ఇమెయిల్ పంపండి: { $literal1 }. మేము సహాయం చేస్తాము.

linux.panel.couldnotreachthedesktoptocheckwhatitneeds = దానికి ఏమి కావాలో సరిచూడటానికి డెస్క్‌టాప్‌ను చేరలేకపోయింది

linux.panel.couldnotreachthesolstoneappx = solstone యాప్‌ను చేరలేకపోయింది: { $reason }

linux.panel.couldnotreachthisdesktoptofindoutwhatitnee = దీనికి ఏమి కావాలో తెలుసుకోవడానికి ఈ డెస్క్‌టాప్‌ను చేరలేకపోయింది.

linux.panel.donethepaneliconisinyourpanelnow = పూర్తయింది. ప్యానెల్ ఐకాన్ ఇప్పుడు మీ ప్యానెల్‌లో ఉంది.

linux.panel.dontaskagain = మళ్ళీ అడగకండి

linux.panel.erroractivityhelperassetsdirectorynotfound = లోపం: కార్యకలాప సహాయకం assets డైరెక్టరీ దొరకలేదు

linux.panel.errorfailedtocopygnomeactivityhelperx = లోపం: { $literal1 } కార్యకలాప సహాయకం కాపీ సాధ్యం కాలేదు: { $err }

linux.panel.errorfailedtocopykwinactivityhelperx = లోపం: { $literal1 } కార్యకలాప సహాయకం కాపీ సాధ్యం కాలేదు: { $err }

linux.panel.errornoactivityhelperassetsfoundinx = లోపం: { $source } లో కార్యకలాప సహాయకం assets దొరకలేదు

linux.panel.gnomegaveananswerwedonotknowx = { $literal1 } మాకు తెలియని సమాధానం ఇచ్చింది: { $other }

linux.panel.gnomeinstalledtheextensionbutcouldnotturnit = { $literal1 } ఎక్స్‌టెన్షన్‌ను ఇన్‌స్టాల్ చేసింది, కానీ ఆన్ చేయలేకపోయింది

linux.panel.gnomeissetnottoloadextensionssothepanelico = { $literal1 } ఎక్స్‌టెన్షన్‌లు లోడ్ చేయకూడదని సెట్ అయి ఉంది, కాబట్టి ప్యానెల్ ఐకాన్ ఈ దారిన తిరిగి రాలేదు

linux.panel.gnomeneedsoneextensionbeforethepaneliconcan = ప్యానెల్ ఐకాన్ కనిపించే ముందు { $literal1 } కు ఒక ఎక్స్‌టెన్షన్ కావాలి. పాజ్ మరియు కొనసాగించండి అక్కడ ఉంటాయి.

linux.panel.gnometooktheextensionbutthepaneliconhasnot = { $literal1 } ఎక్స్‌టెన్షన్‌ను తీసుకుంది, కానీ ప్యానెల్ ఐకాన్ కనిపించలేదు. అది ఎక్కడికి వెళ్ళిందో చూడటానికి { $literal2 } doctor రన్ చేయండి.

linux.panel.gnometooktheextensionbutthepaneliconhasnot2 = { $literal1 } ఎక్స్‌టెన్షన్‌ను తీసుకుంది, కానీ ప్యానెల్ ఐకాన్ ఇంకా కనిపించలేదు. ఒక క్షణం ఇచ్చి, తర్వాత ఇది రన్ చేయండి: { $literal2 } doctor

linux.panel.gnomewillaskyoutoconfirmacceptittoaddthep = { $literal1 } నిర్ధారించమని అడుగుతుంది. ప్యానెల్ ఐకాన్ ఎక్స్‌టెన్షన్‌ను చేర్చడానికి దాన్ని అంగీకరించండి.

linux.panel.gnomewouldnotturntheextensionon = { $literal1 } ఎక్స్‌టెన్షన్‌ను ఆన్ చేయలేదు

linux.panel.installedactivityhelperassetstouserdatadirect = కార్యకలాప సహాయకం assets యూజర్ డేటా డైరెక్టరీలలో ఇన్‌స్టాల్ అయ్యాయి.

linux.panel.logoutandbackinthenrunthisagain = లాగ్ అవుట్ చేసి మళ్ళీ లాగిన్ చేసి, దీన్ని మళ్ళీ రన్ చేయండి.

linux.panel.nochangeyoucanrunthisagainanytime = మార్పు లేదు. దీన్ని ఎప్పుడైనా మళ్ళీ రన్ చేయవచ్చు.

linux.panel.noinstalledappindicatorextensiontoturnon = ఆన్ చేయడానికి ఇన్‌స్టాల్ అయిన appindicator ఎక్స్‌టెన్షన్ లేదు

linux.panel.noteyoumustexplicitlyenabletheextensionorscr = గమనిక: మీ డెస్క్‌టాప్ సెషన్‌లో ఎక్స్‌టెన్షన్‌ను లేదా స్క్రిప్ట్‌ను మీరే ఎనేబుల్ చేయాలి.

linux.panel.nothingherecansetupapanelicononthisdesktop = ఈ డెస్క్‌టాప్‌లో ప్యానెల్ ఐకాన్‌ను సెటప్ చేయగలిగేది ఇక్కడ ఏమీ లేదు.

linux.panel.nothingtodo = చేయాల్సింది ఏమీ లేదు.

linux.panel.nothingtosetuphere = ఇక్కడ సెటప్ చేయాల్సింది ఏమీ లేదు.

linux.panel.notnow = ఇప్పుడు కాదు

linux.panel.notrunning = solstone యాప్ నడవడం లేదు, కాబట్టి పాజ్ చేయడానికి లేదా కొనసాగించడానికి ఏమీ లేదు.

linux.panel.pausedforxminutesrunsolstonelinuxresumetoe = { $minutes } నిమిషం(లు) పాజ్ అయి ఉంది. ముందే ఆపడానికి { $literal1 } resume రన్ చేయండి.

linux.panel.pausedrunsolstonelinuxresumewhenyouwantitba = పాజ్ అయి ఉంది. తిరిగి కావాలంటే { $literal1 } resume రన్ చేయండి.

linux.panel.setitup = దీన్ని సెటప్ చేయండి

linux.panel.solstoneapp = solstone యాప్

linux.panel.thedownloadedextensioncouldnotbeunpacked = డౌన్‌లోడ్ అయిన ఎక్స్‌టెన్షన్‌ను విప్పలేకపోయింది

linux.panel.theextensioncouldnotbedownloadedcheckthenetw = ఎక్స్‌టెన్షన్‌ను డౌన్‌లోడ్ చేయలేకపోయింది; నెట్‌వర్క్ కనెక్షన్‌ను సరిచూడండి

linux.panel.thegnomeextensionisalreadyonifthepanelicon = { $literal1 } ఎక్స్‌టెన్షన్ ఇప్పటికే ఆన్‌లో ఉంది. ప్యానెల్ ఐకాన్ ఇంకా లేకపోతే, లాగ్ అవుట్ చేసి మళ్ళీ లాగిన్ చేయండి.

linux.panel.thegnomeextensionisinstalledbutturnedoffturn = { $literal1 } ఎక్స్‌టెన్షన్ ఇన్‌స్టాల్ అయి ఉంది, కానీ ఆఫ్‌లో ఉంది. ఇప్పుడు ఆన్ చేస్తోంది.

linux.panel.thepaneliconextensionisinstalledbutgnomehas = ప్యానెల్ ఐకాన్ ఎక్స్‌టెన్షన్ ఇన్‌స్టాల్ అయి ఉంది, కానీ { $literal1 } ఇంకా దాన్ని తీసుకోలేదు.

linux.panel.thepaneliconextensionisinstalledlogoutandba = ప్యానెల్ ఐకాన్ ఎక్స్‌టెన్షన్ ఇన్‌స్టాల్ అయి ఉంది. లాగ్ అవుట్ చేసి మళ్ళీ లాగిన్ చేసి, { $literal1 } మళ్ళీ రన్ చేయండి.

linux.panel.thepaneliconisalreadyavailableonthisdesktop = ప్యానెల్ ఐకాన్ ఈ డెస్క్‌టాప్‌లో ఇప్పటికే అందుబాటులో ఉంది.

linux.panel.thepaneliconisnotsetupx = ప్యానెల్ ఐకాన్ సెటప్ కాలేదు: { $reason }

linux.panel.thepaneliconisreadypauseandresumeareinthe = ప్యానెల్ ఐకాన్ సిద్ధంగా ఉంది. పాజ్ మరియు కొనసాగించండి ఇప్పుడు ప్యానెల్‌లో ఉన్నాయి.

linux.panel.thesolstoneappisnotrunningsothereisnothing = solstone యాప్ నడవడం లేదు, కాబట్టి పాజ్ చేయడానికి లేదా కొనసాగించడానికి ఏమీ లేదు.

linux.panel.thisdesktopdoesnotallowextensionstobeinstall = ఈ డెస్క్‌టాప్ ఎక్స్‌టెన్షన్‌లు ఇన్‌స్టాల్ కావడాన్ని అనుమతించదు

linux.panelicon.resumed = కొనసాగింది.

linux.screenshots.bringcount = { $count ->
    [one] 1 స్క్రీన్‌షాట్ తీసుకురండి
   *[other] { $formatted } స్క్రీన్‌షాట్‌లు తీసుకురండి
}

linux.screenshots.refusedcount = { $count ->
    [one] 1 స్క్రీన్‌షాట్ లోపలికి రాలేదు, కాబట్టి అది ఫోల్డర్‌లోనే ఉంటుంది.
   *[other] { $formatted } స్క్రీన్‌షాట్‌లు లోపలికి రావు, కాబట్టి అవి ఫోల్డర్‌లోనే ఉంటాయి.
}

linux.screenshots.total = { $count ->
    [one] 1 స్క్రీన్‌షాట్
   *[other] { $formatted } స్క్రీన్‌షాట్‌లు
}

linux.screenshots.unreadablecount = { $count ->
    [one] 1 స్క్రీన్‌షాట్ చదవలేకపోయింది.
   *[other] { $formatted } స్క్రీన్‌షాట్‌లు చదవలేకపోయింది.
}

linux.service.currentexecutablehasnoparentdirectory = ప్రస్తుత ఎగ్జిక్యూటబుల్‌కు పేరెంట్ డైరెక్టరీ లేదు

linux.service.errorremovingxx = { $value1 } తీసివేయడంలో లోపం: { $error }

linux.service.errorwritingservicefilesx = సర్వీస్ ఫైల్‌లు రాయడంలో లోపం: { $error }

linux.service.homeisnotsetcannotlocateuserservicefiles = HOME సెట్ కాలేదు; యూజర్ సర్వీస్ ఫైల్‌లు దొరకలేదు

linux.service.invalidservicepid = చెల్లని సర్వీస్ PID

linux.service.removed = { $value1 } తీసివేయబడింది

linux.service.servicepidunavailable = సర్వీస్ PID అందుబాటులో లేదు

linux.service.servicerestartfailedx = సర్వీస్ రీస్టార్ట్ విఫలమైంది: { $error }

linux.service.systemctlcouldnotrestartsolstonelinuxservicex = systemctl { $literal1 }.service రీస్టార్ట్ చేయలేకపోయింది:{ $ws2 }{ $value1 }

linux.service.systemctlnotfound = systemctl దొరకలేదు

linux.service.systemctlstopfailed = systemctl ఆపడం విఫలమైంది

linux.service.warningcouldnotremoveerror = హెచ్చరిక: { $value1 } తీసివేయలేకపోయింది: { $error }

linux.service.warningcouldnotremoveeverybrowserregistration = హెచ్చరిక: ప్రతి బ్రౌజర్ రిజిస్ట్రేషన్‌ను తీసివేయలేకపోయింది: { $error }

linux.service.warningcouldnotwriteeverybrowserregistrationx = హెచ్చరిక: ప్రతి బ్రౌజర్ రిజిస్ట్రేషన్‌ను రాయలేకపోయింది: { $error }

linux.service.warningsystemctlenablenowfailedrunsystemctlus = హెచ్చరిక: systemctl enable --now విఫలమైంది; లోపం పరిశీలించడానికి '{ $literal1 }' రన్ చేయండి

linux.service.warningsystemctlenablenowfailedxrunsystemctl = హెచ్చరిక: systemctl enable --now విఫలమైంది: { $error }; systemd అందుబాటులో ఉన్న తర్వాత '{ $literal1 }' రన్ చేయండి

linux.service.warningsystemctlstartfailedrunsystemctluserst = హెచ్చరిక: systemctl start విఫలమైంది; లోపం పరిశీలించడానికి '{ $literal1 }' రన్ చేయండి

linux.service.warningsystemctlstartfailedxrunsystemctluser = హెచ్చరిక: systemctl start విఫలమైంది: { $error }; systemd అందుబాటులో ఉన్న తర్వాత '{ $literal1 }' రన్ చేయండి

linux.service.warningsystemctlxfailedrunsystemctluserxtoi = హెచ్చరిక: systemctl { $operation } విఫలమైంది; లోపం పరిశీలించడానికి '{ $literal1 } { $operation }' రన్ చేయండి

linux.service.warningsystemctlxfailedxrunsystemctluserxaf = హెచ్చరిక: systemctl { $operation } విఫలమైంది: { $error }; systemd అందుబాటులో ఉన్న తర్వాత '{ $literal1 } { $operation }' రన్ చేయండి

linux.service.wrotex = { $value1 } రాయబడింది

linux.shared.cli.versionlastknown = { $value1 } (చివరిసారి తెలిసింది)

linux.shared.copy.connected = కనెక్ట్ అయి ఉంది

linux.shared.copy.status = స్థితి

linux.shared.copy.yourjournalshowsthissamemarkinitsnetworkapp = మీ జర్నల్ తన నెట్‌వర్క్ యాప్‌లో ఇదే మార్క్ చూపిస్తుంది. అది కచ్చితంగా సరిపోవాలి.

linux.shared.health.unknown = తెలియదు

linux.shared.pair.confirmcantaskyouaboutyourjournalsmarkhere = confirm ఇక్కడ మీ జర్నల్ మార్క్ గురించి అడగలేదు. టర్మినల్ లో దీన్ని రన్ చేయండి, లేదా --mark "word word" తో మార్క్ యొక్క రెండు పదాలు ఇవ్వండి.

linux.shared.pair.couldntverify = ధృవీకరించలేకపోయింది.

linux.shared.pair.doesthismatchyourjournaltypeyesorno = ఇది మీ జర్నల్‌తో సరిపోతుందా? { $literal1 } లేదా { $literal2 } టైప్ చేయండి:{ $ws3 }

linux.shared.pair.givethetwowordsofyourjournalsmarklikemark = మీ జర్నల్ మార్క్ యొక్క రెండు పదాలు ఇవ్వండి, { $literal1 } లాగా.

linux.shared.pair.notpaired = పెయిర్ కాలేదు.

linux.shared.pair.setupcantaskyouaboutyourjournalsmarkherer = setup ఇక్కడ మీ జర్నల్ మార్క్ గురించి అడగలేదు. టర్మినల్ లో దీన్ని రన్ చేయండి, లేదా --mark "word word" తో మార్క్ యొక్క రెండు పదాలు ఇవ్వండి.

linux.shared.pair.thetwowordsofthemarkyourjournalsnetworkapp = మీ జర్నల్ నెట్‌వర్క్ యాప్ చూపించే మార్క్ యొక్క రెండు పదాలు. అడగడానికి టర్మినల్ లేనప్పుడు ఇది కావాలి.

linux.shared.pair.typecontinuetopairanywayorcanceltostop = అయినా పెయిర్ చేయడానికి { $literal1 } టైప్ చేయండి, లేదా ఆపడానికి { $literal2 }:{ $ws3 }

linux.shared.pair.waitingforyoutoconfirmyourjournalsmarknothi = మీ జర్నల్ మార్క్‌ను మీరు నిర్ధారించేందుకు వేచి ఉంది. వేచి ఉన్నది ఏదీ, మీరు నిర్ధారించే వరకు, మీ జర్నల్‌లోకి వెళ్ళదు.

linux.shared.pair.yourjournalsmarkisalreadyconfirmednothingto = మీ జర్నల్ మార్క్ ఇప్పటికే నిర్ధారించబడింది. చేయాల్సింది ఏమీ లేదు.

linux.shared.tray.offline = ఆఫ్‌లైన్

linux.sharedsources.also = వీటిలో కూడా

linux.sharedsources.cli = Obsidian: { $state }

linux.sharedsources.many = ఇతర { $count } పరికరాల నుండి

linux.sharedsources.one = మరో పరికరం నుండి

linux.sharedsources.pair = { $first } · { $second }

linux.shell.activitystillon = కార్యకలాపం ఇంకా ఆన్‌లో ఉంది.

linux.shell.afternoon = శుభ మధ్యాహ్నం

linux.shell.amountsince = { $date } నుండి { $total }

linux.shell.attention = శ్రద్ధ కావాలి

linux.shell.browserunavailable = బ్రౌజర్ పేజీలు ఇప్పుడు స్టార్ట్ అవ్వలేవు.

linux.shell.checking = సరిచూస్తోంది…

linux.shell.evening = శుభ సాయంత్రం

linux.shell.hourcount = { $count ->
    [one] { $display } గంట
   *[other] { $display } గంటలు
}

linux.shell.hoursago = { $count ->
    [one] { $display } గంట క్రితం
   *[other] { $display } గంటల క్రితం
}

linux.shell.intakeunreadable = ఎంత తీసుకోలు అయిందో దాని నమోదు చదవలేకపోయింది.

linux.shell.itemcount = { $count ->
    [one] { $display } అంశం
   *[other] { $display } అంశాలు
}

linux.shell.lessthanminute = ఒక నిమిషం కంటే తక్కువ

linux.shell.lessthanminuteleft = పాజ్ అయి ఉంది, ఒక నిమిషం కంటే తక్కువ మిగిలి ఉంది

linux.shell.minutecount = { $count ->
    [one] { $display } నిమిషం
   *[other] { $display } నిమిషాలు
}

linux.shell.minutesago = { $minutes } నిమిషాల క్రితం

linux.shell.month.april = ఏప్రిల్

linux.shell.month.august = ఆగస్టు

linux.shell.month.december = డిసెంబర్

linux.shell.month.february = ఫిబ్రవరి

linux.shell.month.january = జనవరి

linux.shell.month.july = జూలై

linux.shell.month.june = జూన్

linux.shell.month.march = మార్చి

linux.shell.month.may = మే

linux.shell.month.november = నవంబర్

linux.shell.month.october = అక్టోబర్

linux.shell.month.september = సెప్టెంబర్

linux.shell.monthday = { $day } { $month }

linux.shell.monthdayyear = { $day } { $month }, { $year }

linux.shell.morning = శుభోదయం

linux.shell.notecount = { $count ->
    [one] { $display } నోట్
   *[other] { $display } నోట్‌లు
}

linux.shell.pagecount = { $count ->
    [one] { $display } పేజీ
   *[other] { $display } పేజీలు
}

linux.shell.pauseleft = పాజ్ అయి ఉంది, { $minutes } ని మిగిలి ఉంది

linux.shell.readytosetup = సెటప్ చేయడానికి సిద్ధంగా ఉంది

linux.shell.screenmissingplugin = { $literal1 } ప్లగిన్ లేదు.

linux.shell.screensharingnotallowed = స్క్రీన్ షేరింగ్ అనుమతించబడలేదు.

linux.shell.screenshotcount = { $count ->
    [one] { $display } స్క్రీన్‌షాట్
   *[other] { $display } స్క్రీన్‌షాట్‌లు
}

linux.shell.screenstartfailed = స్క్రీన్ తీసుకోలు స్టార్ట్ కాలేకపోయింది.

linux.shell.settingup = సెటప్ చేస్తోంది

linux.shell.terminalunavailable = టర్మినల్ సెషన్‌లు ఇప్పుడు స్టార్ట్ కాలేదు.

linux.shell.tileneedsattention = { $source } కు శ్రద్ధ కావాలి

linux.shell.untilresume = మీరు కొనసాగించే వరకు పాజ్ అయి ఉంది

linux.shell.withaudio = ఆడియోతో

linux.shell.withaudioactivity = ఆడియో మరియు కార్యకలాపంతో

linux.shell.yesterday = నిన్న

linux.state.stoppedbyanerrorrestarting = లోపం వల్ల ఆగింది, రీస్టార్ట్ అవుతోంది

linux.state.terminalisoff = టర్మినల్ ఆఫ్‌లో ఉంది

linux.state.tmuxnotfound = { $literal1 } దొరకలేదు

linux.state.waitingsolstonefortmuxisstillinstalled = వేచి ఉంది: solstone for { $literal1 } ఇంకా ఇన్‌స్టాల్ అయి ఉంది. యాప్‌లో టర్మినల్ ఆన్ చేయండి, లేదా దీన్ని ఇక్కడికి తరలించడానికి `{ $literal2 } terminal on` రన్ చేయండి

linux.status.browsernobrowserconnected = బ్రౌజర్: ఏ బ్రౌజర్ కనెక్ట్ కాలేదు

linux.status.browserthesolstoneappisnotrunning = బ్రౌజర్: solstone యాప్ నడవడం లేదు

linux.status.heldxsegmentsnotsent = నిలిచి ఉంది: { $value1 } సెగ్మెంట్(లు) పంపబడలేదు

linux.status.heldxsegmentsnotsentoldestxd = నిలిచి ఉంది: { $value1 } సెగ్మెంట్(లు) పంపబడలేదు, పాతది { $value2 }d

linux.status.pausedsononewpagesaretaken = { $ws1 }పాజ్ అయి ఉంది, కాబట్టి కొత్త పేజీలు లోపలికి రావు

linux.status.theroomforbrowserpagesonthis = { $ws1 }ఈ కంప్యూటర్‌లో బ్రౌజర్ పేజీలకు చోటు నిండింది, కాబట్టి కొన్ని మీ జర్నల్‌లోకి వెళ్ళే వరకు కొత్త పేజీలు లోపలికి రావు

linux.sync.checkingjournal = journal సరిచూస్తోంది...

linux.sync.checkingx = { $day } సరిచూస్తోంది...

linux.sync.probefailednextinxs = { $count ->
    [one] ప్రోబ్ విఫలమైంది, తర్వాత { $count } సెకనులో
   *[other] ప్రోబ్ విఫలమైంది, తర్వాత { $count } సెకన్లలో
}

linux.sync.probingjournal = journal ప్రోబ్ చేస్తోంది...

linux.sync.syncing = సింక్ అవుతోంది...

linux.sync.uploadingx = { $segment-key } అప్‌లోడ్ అవుతోంది

linux.sync.xsuntilprobe = { $count ->
    [one] ప్రోబ్ వరకు { $count } సెకను
   *[other] ప్రోబ్ వరకు { $count } సెకన్లు
}

linux.synchealth.ifyourecentlyresetjournal = మీరు ఇటీవల మీ జర్నల్‌ను రీసెట్ చేసి ఉంటే, ఈ పరికరాన్ని మళ్ళీ పెయిర్ చేయండి.

linux.synchealth.thisislesscertainthan = ఇది ప్రత్యక్ష కనెక్షన్ కంటే తక్కువ ఖచ్చితం.

linux.terminal.control.bringold = వేచి ఉన్నది తెస్తోంది

linux.terminal.control.confirmationsolstoneappisnt = { $confirmation } solstone యాప్ నడవడం లేదు; అది స్టార్ట్ అయినప్పుడు ఇది అమలు అవుతుంది.

linux.terminal.control.couldnotmovesolstonetmux = solstone for { $literal1 } లోపలికి తరలించలేకపోయింది: { $error }

linux.terminal.control.couldnotreachsolstoneapp = solstone యాప్‌ను చేరలేకపోయింది: { $error }

linux.terminal.control.couldnotrecordmoveyet = తరలింపు ఇంకా నమోదు కాలేదు: { $error }

linux.terminal.control.couldnotsavethaterror = అది సేవ్ కాలేదు: { $error }

linux.terminal.control.couldntuseproblem = { $ws1 }{ $value1 } వాడలేకపోయింది: { $problem }

linux.terminal.control.ignoredsettingkey = { $ws1 }విస్మరించిన సెట్టింగ్: { $key }

linux.terminal.control.isoff = టర్మినల్ ఆఫ్‌లో ఉంది.

linux.terminal.control.ison = టర్మినల్ ఆన్‌లో ఉంది.

linux.terminal.control.morethanoneoldpairing = { $ws1 }ఒకటి కంటే ఎక్కువ పాత పెయిరింగ్: { $value1 }

linux.terminal.control.moving = solstone for tmux లోపలికి తరలుతోంది.

linux.terminal.control.movingsolstonetmuxneedsok = solstone for { $literal1 } లోపలికి తరలడానికి మీ ok కావాలి. టర్మినల్ లో దీన్ని రన్ చేయండి, లేదా `terminal adopt-{ $literal1 }` కు --yes చేర్చండి.

linux.terminal.control.movingsolstonetmuxphase = { $ws1 }solstone for { $literal2 } లోపలికి తరలుతోంది: { $phase }

linux.terminal.control.newterminalhistoryfromhere = { $ws1 }ఇక్కడి నుండి కొత్త టర్మినల్ చరిత్ర, మరియు మీ జర్నల్‌లో ఇప్పటికే ఉన్నది అక్కడే ఉంటుంది ({ $reason })

linux.terminal.control.nosolstonetmuxmove = లోపలికి తరలించడానికి solstone for { $literal1 } లేదు.

linux.terminal.control.oldsegmentsheldback = { $ws1 }{ $value1 } పాత సెగ్మెంట్(లు) నిలిచి ఉన్నాయి

linux.terminal.control.problem = { $ws1 }సమస్య: { $value1 }

linux.terminal.control.recordmovingsolstonetmuxcan = solstone for { $literal1 } తరలింపు నమోదు చదవలేదు: { $value1 }. దయచేసి సపోర్ట్‌ను సంప్రదించండి, దాన్ని తొలగించకండి.

linux.terminal.control.screenaudiorestartappafter = స్క్రీన్ మరియు ఆడియో: ఇక్కడ మీ డెస్క్‌టాప్‌లో లాగిన్ చేసిన తర్వాత యాప్‌ను రీస్టార్ట్ చేయండి

linux.terminal.control.screenisoff = స్క్రీన్, ఆడియో మరియు డెస్క్‌టాప్ కార్యకలాపం ఆఫ్‌లో ఉన్నాయి.

linux.terminal.control.screenison = స్క్రీన్, ఆడియో మరియు డెస్క్‌టాప్ కార్యకలాపం ఆన్‌లో ఉన్నాయి.

linux.terminal.control.screenoffstatus = స్క్రీన్, ఆడియో మరియు డెస్క్‌టాప్ కార్యకలాపం: ఆఫ్

linux.terminal.control.segmentsfromsolstonetmux = { $ws1 }solstone for { $literal2 } నుండి { $value1 } సెగ్మెంట్(లు) అప్‌లోడ్ కావడానికి వేచి ఉన్నాయి

linux.terminal.control.segmentsheld = { $ws1 }{ $value1 } సెగ్మెంట్(లు) నిలిచి ఉన్నాయి

linux.terminal.control.segmentswaitingupload = { $ws1 }{ $value1 } సెగ్మెంట్(లు) అప్‌లోడ్ కావడానికి వేచి ఉన్నాయి

linux.terminal.control.solstoneappdidnotanswer = solstone యాప్ సమయానికి సమాధానం ఇవ్వలేదు; ఒక క్షణం తర్వాత మళ్ళీ ప్రయత్నించండి

linux.terminal.control.solstoneappdidnotanswer2 = solstone యాప్ సమాధానం ఇవ్వలేదు, కాబట్టి ఏమీ మారలేదు; ఒక క్షణం తర్వాత మళ్ళీ ప్రయత్నించండి.

linux.terminal.control.solstoneappisstartingtry = solstone యాప్ స్టార్ట్ అవుతోంది; ఒక క్షణం తర్వాత మళ్ళీ ప్రయత్నించండి.

linux.terminal.control.solstoneappisstillmoving = solstone యాప్ ఇంకా solstone for { $literal1 } లోపలికి తరలిస్తోంది; అది దానంతట అదే పూర్తి అవుతుంది.

linux.terminal.control.solstoneappneedsrestartingbefore = solstone for { $literal1 } లోపలికి తరలించే ముందు solstone యాప్‌ను రీస్టార్ట్ చేయాలి.

linux.terminal.control.solstonetmuxhasalreadymoved = solstone for { $literal1 } ఇప్పటికే లోపలికి తరలింది.

linux.terminal.control.solstonetmuxhasmoved = solstone for { $literal1 } లోపలికి తరలింది.

linux.terminal.control.solstonetmuxhasmovedsolstone = solstone for { $literal1 } లోపలికి తరలింది. solstone యాప్ స్టార్ట్ అయినప్పుడు మిగతాది పూర్తి చేస్తుంది.

linux.terminal.control.solstonetmuxisstillinstalled = solstone for { $literal1 } ఈ కంప్యూటర్‌లో ఇంకా ఇన్‌స్టాల్ అయి ఉంది, కాబట్టి టర్మినల్ ఇప్పటికీ ఆఫ్‌లోనే ఉంటుంది

linux.terminal.control.stopold = పాత యాప్ ఆగుతోంది

linux.terminal.control.thismovessolstonetmuxinto = solstone for { $literal1 } ఈ యాప్‌లోకి తరలుతుంది, అది శాశ్వతంగా ఆగుతుంది. వేచి ఉన్నది ఈ యాప్ ద్వారా మీ జర్నల్‌లోకి వెళ్తుంది, వెళ్ళలేనిది ఈ కంప్యూటర్‌లోనే ఉంటుంది. ఈ యాప్ ఇంకా పెయిర్ కాకపోతే, solstone for { $literal1 } వాడే journalతో పెయిర్ అవుతుంది. ఇప్పుడు తరలించాలా? [y/N]

linux.terminal.control.tmux = { $ws1 }{ $literal2 }: { $value1 }

linux.terminal.control.unfinishedsegmentswaitingterminal = { $ws1 }{ $value1 } పూర్తి కాని సెగ్మెంట్(లు) టర్మినల్ మళ్ళీ నడిచే వరకు వేచి ఉన్నాయి

linux.terminal.control.waitinganswer = { $ws1 }వేచి ఉంది: { $answer }

linux.terminal.malformedcommand = ఆకృతి తప్పు అయిన కమాండ్

linux.terminal.state.stoppedbyerrorrestartingerror = లోపం వల్ల ఆగింది, రీస్టార్ట్ అవుతోంది: { $error }

linux.terminal.takeover.couldnotremoveerror = { $value1 } తీసివేయలేకపోయింది: { $error }

linux.terminal.takeover.isnotsolstonetmuxservice = { $value1 } solstone for { $literal1 } సర్వీస్ కాదు, కాబట్టి ఏమీ మారలేదు.

linux.terminal.takeoverrunner.couldnotsaveterminalsettings = టర్మినల్ సెట్టింగ్‌లు సేవ్ కాలేదు: { $error }

linux.terminal.takeoverrunner.couldnotturnterminalerror = టర్మినల్ ఆన్ చేయలేకపోయింది: { $error }

linux.terminal.warning = హెచ్చరిక: { $detail }

linux.tray.aboutsolstone = solstone గురించి

linux.tray.cachexmb = కాష్: { $value1 } MB

linux.tray.cantcheckwhichsessionisinfront = ఏ సెషన్ ముందు ఉందో సరిచూడలేకపోయింది

linux.tray.cantreachyourdesktop = మీ డెస్క్‌టాప్‌ను చేరలేకపోయింది

linux.tray.connectedallcaughtup = కనెక్ట్ అయి ఉంది · తాజాగా ఉంది

linux.tray.idlescreeninactive = పని లేకుండా (స్క్రీన్ యాక్టివ్ కాదు)

linux.tray.n15minutes = 15 నిమిషాలు

linux.tray.n1hour = 1 గంట

linux.tray.n30minutes = 30 నిమిషాలు

linux.tray.notpaired = పెయిర్ కాలేదు

linux.tray.notrunning = నడవడం లేదు

linux.tray.offlinewaiting = ఆఫ్‌లైన్ · { $count } వేచి ఉంది

linux.tray.openjournal = జర్నల్ తెరవండి

linux.tray.opensettings = సెట్టింగ్‌లు తెరవండి

linux.tray.pausedxmremaining = { $count ->
    [one] పాజ్ అయి ఉంది ({ $count } నిమిషం మిగిలి ఉంది)
   *[other] పాజ్ అయి ఉంది ({ $count } నిమిషాలు మిగిలి ఉంది)
}

linux.tray.quitsolstone = solstone నుండి నిష్క్రమించండి

linux.tray.resume = కొనసాగించండి

linux.tray.resumexmremaining = { $count ->
    [one] కొనసాగించండి ({ $count } నిమిషం మిగిలి ఉంది)
   *[other] కొనసాగించండి ({ $count } నిమిషాలు మిగిలి ఉంది)
}

linux.tray.screenintakecantstart = స్క్రీన్ తీసుకోలు స్టార్ట్ కాలేదు

linux.tray.screenintakecouldntstart = స్క్రీన్ తీసుకోలు స్టార్ట్ కాలేకపోయింది

linux.tray.screenneedsattention = స్క్రీన్‌కు శ్రద్ధ కావాలి

linux.tray.screensharingnotallowed = స్క్రీన్ షేరింగ్ అనుమతించబడలేదు

linux.tray.tooltip = { $status }{ $break }{ $health }

linux.tray.untiliresume = నేను కొనసాగించే వరకు

linux.tray.waitingforscreenintake = స్క్రీన్ తీసుకోలు కోసం వేచి ఉంది

linux.tray.waitingforyourdesktop = మీ డెస్క్‌టాప్ కోసం వేచి ఉంది

linux.tray.waitingnoneofyoursessionsisinfront = వేచి ఉంది (మీ సెషన్‌లలో ఏదీ ముందు లేదు)

linux.tray.waitingyoursessionisntinfront = వేచి ఉంది (మీ సెషన్ ముందు లేదు)

linux.ui.terminalterminalstatus = టర్మినల్: { $terminal-status }

linux.window.addmore = మరిన్ని చేర్చండి

linux.window.allcaughtup = తాజాగా ఉంది

linux.window.brightharbor = bright · harbor

linux.window.browsers = బ్రౌజర్‌లు

linux.window.browsersbreath = మీరు అనుమతించిన సైట్‌లలో, solstone ఎక్స్‌టెన్షన్ పేజీ వచనాన్ని మరియు సుమారు లేఅవుట్‌ను స్వీకరిస్తుంది, మరియు అంతా మీ జర్నల్‌లోకి వెళుతుంది.

linux.window.browsersgap = పేజీలు solstone ఎక్స్‌టెన్షన్ నుండి వస్తాయి. దానికి ఇక్కడ స్విచ్ లేదు.

linux.window.cancel = రద్దు చేయండి

linux.window.checkingyourjournal = మీ జర్నల్‌ను సరిచూస్తోంది.

linux.window.choosefiles = ఫైల్‌లు ఎంచుకోండి…

linux.window.choosenewdownloadname = ఉన్న ఫైల్‌ను ఉంచడానికి కొత్త పేరు ఎంచుకోండి

linux.window.close = మూసివేయండి

linux.window.connectajournal = ఒక జర్నల్‌ను కనెక్ట్ చేయండి

linux.window.connected = కనెక్ట్ అయి ఉంది

linux.window.connection = కనెక్షన్

linux.window.everythinginyourjournal = అంతా మీ జర్నల్‌లో ఉంది

linux.window.files = ఫైల్‌లు

linux.window.foregroundtitleurlidle = { $route }; ఫోర్‌గ్రౌండ్ { $foreground }; శీర్షిక { $title }; లింక్ { $url }; పని లేకుండా { $idle }

linux.window.goesalongwithit = దానితో పాటు వస్తుంది

linux.window.help = సహాయం

linux.window.home = ‹ హోమ్

linux.window.home.vcb176487 = హోమ్

linux.window.import = ఇంపోర్ట్

linux.window.keptonthiscomputer = ఈ కంప్యూటర్‌లో ఉంచబడింది

linux.window.kilobytes = { $size } KB

linux.window.lastreached = చివరిసారి చేరింది

linux.window.lasttakenin = చివరి స్వీకరణ

linux.window.leftout = మినహాయించబడింది

linux.window.megabytes = { $size } MB

linux.window.nothingyet = ఇంకా ఏమీ లేదు

linux.window.notpaired = పెయిర్ కాలేదు

linux.window.obsidian = { $literal1 }

linux.window.offsub = ఎప్పుడైనా ఆన్ చేయండి.

linux.window.ok = ok

linux.window.onthisdevice = ఈ పరికరంలో

linux.window.pause = పాజ్ చేయండి ▾

linux.window.recent = ఇటీవలి

linux.window.remove = తీసివేయండి

linux.window.remove.vf022f0fa = { $value1 } తీసివేయాలా?

linux.window.resume = కొనసాగించండి

linux.window.screen = స్క్రీన్

linux.window.screenaudioandactivitystillon = ఆడియో మరియు కార్యకలాపం ఇంకా ఆన్‌లో ఉన్నాయి.

linux.window.screenaudiostillon = ఆడియో ఇంకా ఆన్‌లో ఉంది.

linux.window.screenbreath = ఇది ఆన్‌లో ఉన్నప్పుడు, మీ స్క్రీన్‌లో ఉన్నది, ధ్వని, మరియు మీరు వాడుతున్న యాప్‌లు మరియు విండోలు solstone యాప్ స్వీకరిస్తుంది, అదంతా మీ జర్నల్‌లోకి వెళ్తుంది.

linux.window.screenloginunavailable = ఏ సెషన్ ముందు ఉందో సరిచూడలేకపోయింది.

linux.window.screenmissingelements = స్క్రీన్ తీసుకోలు స్టార్ట్ కాలేదు: { $literal1 } లో { $elements } లేవు. ప్యాకేజీ మేనేజర్‌తో { $literal1 } { $literal2 }, good మరియు base ప్లగిన్‌లు ఇన్‌స్టాల్ చేసి, మళ్ళీ ప్రయత్నించండి ఎంచుకోండి.

linux.window.screenmissingelementsx11 = స్క్రీన్ తీసుకోలు స్టార్ట్ కాలేదు: { $literal1 } లో { $elements } లేవు. ప్యాకేజీ మేనేజర్‌తో { $literal1 } good మరియు base ప్లగిన్‌లు ఇన్‌స్టాల్ చేసి, మళ్ళీ ప్రయత్నించండి ఎంచుకోండి.

linux.window.screenretrying = స్క్రీన్ తీసుకోలు మొదటి ప్రయత్నంలో స్టార్ట్ కాలేదు. ఒక క్షణం తర్వాత మళ్ళీ ప్రయత్నిస్తోంది.

linux.window.screensharingnotallowed = స్క్రీన్ షేరింగ్ అనుమతించబడలేదు. మీ స్క్రీన్‌ను పంచుకోవడానికి మళ్ళీ ప్రయత్నించండి ఎంచుకోండి.

linux.window.screensound = ధ్వనిలో మీ మైక్రోఫోన్, మరియు ఈ కంప్యూటర్ ప్లే చేసేది, ఉంటాయి. మైక్రోఫోన్ లేకపోతే రెండూ లోపలికి రావు.

linux.window.screenstartfailed = స్క్రీన్ తీసుకోలు స్టార్ట్ కాలేకపోయింది.

linux.window.screenunreachable = మీ డెస్క్‌టాప్‌ను చేరలేకపోయింది.

linux.window.screenwaiting = స్క్రీన్‌ను తీసుకోవడానికి వేచి ఉంది.

linux.window.sidebar = సైడ్‌బార్

linux.window.sofar = ఇప్పటి వరకు

linux.window.somethingyouwereaddingdidntfinish = మీరు చేర్చేది సేవ్ అయిపోలేదు. మళ్ళీ చేర్చండి.

linux.window.sources = మూలాలు

linux.window.status = స్థితి

linux.window.status.vaff1290e = ‹ స్థితి

linux.window.technicaldetails = సాంకేతిక వివరాలు

linux.window.terminal = టర్మినల్

linux.window.terminalbreath = మీ { $literal1 } సెషన్‌లను solstone యాప్ స్వీకరిస్తుంది, అదంతా మీ జర్నల్‌లోకి వెళ్తుంది. ఈ కంప్యూటర్‌లో { $literal1 } కావాలి. మీరు ఈ కంప్యూటర్‌లోకి { $literal2 } చేస్తున్నప్పుడు కూడా, ఏ టర్మినల్ సెషన్‌లోనైనా ఇది పని చేస్తుంది.

linux.window.thecopyisalreadygonesothis = కాపీ ఇప్పటికే పోయింది, కాబట్టి ఇది దాన్ని ఇక్కడి నుండి మాత్రమే తీసివేస్తుంది.

linux.window.thiscomputer = ఈ కంప్యూటర్

linux.window.thisremovesthecopyonthisdevice = ఇది ఈ పరికరంలోని కాపీని తీసివేస్తుంది. మీ జర్నల్‌లో ఇప్పటికే ఉన్న కాపీని అది ఇంకా తీసుకురావచ్చు.

linux.window.thisremovesthecopyonthisdevice.v558a0c4f = ఇది ఈ పరికరంలోని కాపీని తీసివేస్తుంది, కాబట్టి అది మీ జర్నల్‌కు చేరదు.

linux.window.trying = ప్రయత్నిస్తోంది…

linux.window.trynow = ఇప్పుడు ప్రయత్నించండి

linux.window.updates = అప్‌డేట్‌లు

linux.window.yourjournal = మీ జర్నల్

linux.window.yourjournal.vc43fd4d5 = మీ · జర్నల్
