// This expression runs only in the connection's own browser, before connect
// Browser preferences/PI HTML do not change the host application's UI language
(info, browser) => {
    const scale = browser.devicePixelRatio;
    if (typeof scale !== 'number' || !Number.isFinite(scale) || scale <= 0 || scale > 16) {
        throw new Error('Plugin registration requires the actual browser display scale');
    }
    const language = info.application?.language;
    if (!['de', 'en', 'es', 'fr', 'ja', 'ko', 'zh_CN', 'zh_TW'].includes(language)) {
        throw new Error('Plugin registration requires the host application language');
    }
    return { ...info, application: { ...info.application }, devicePixelRatio: scale };
};
