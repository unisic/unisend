package dev.gpui.beeper;

import android.app.Activity;
import android.content.Intent;
import android.content.pm.ApplicationInfo;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.os.Message;
import android.webkit.CookieManager;
import android.webkit.WebChromeClient;
import android.webkit.WebSettings;
import android.webkit.WebStorage;
import android.webkit.WebView;
import android.webkit.WebViewClient;
import java.util.Arrays;
import java.util.Map;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.CountDownLatch;
import org.json.JSONException;
import org.json.JSONObject;
import org.json.JSONTokener;
import java.util.regex.Pattern;
import java.util.regex.PatternSyntaxException;

/**
 * A browser for signing in to a chat network. It closes once the address matches the pattern the
 * bridge gave and every value the bridge needs exists, and hands back the site's cookies, the
 * local storage keys asked for, and what the bridge's own script found, which is what the bridge
 * logs in with. The pattern alone is not enough: Messenger's also matches its own
 * sign-in and captcha pages. {@link #run} is called from Rust over JNI and blocks until then, or
 * until the user closes the page.
 */
public final class WebLogin extends Activity {
    private static CountDownLatch latch;
    private static volatile String result;
    private static String startUrl;
    private static String userAgent;
    private static Pattern done;
    private static String[] domains;
    /**
     * One entry per field the bridge cannot do without, its sources tab-separated:
     * "c:name@domain" a cookie, "s:key" local storage, "f:id" the script's result.
     */
    private static String[] required;
    private static String[] storage;
    /** The bridge's script; it resolves to an object keyed by field ID. */
    private static String script;
    /** Local storage values and the script's result, by key. */
    private static final Map<String, String> found = new ConcurrentHashMap<>();
    /** How often the page is asked for local storage and the script's result. */
    private static final long PROBE_MS = 500;

    private WebView web;
    private final Handler handler = new Handler(Looper.getMainLooper());
    private final Runnable probe = this::probe;

    /** Returns a JSON object of what was found, or null when nothing was. */
    public static String run(
            Activity activity, String url, String agent, String pattern, String domainList, String requiredList,
            String storageList, String extractJs) {
        CountDownLatch wait = new CountDownLatch(1);
        latch = wait;
        result = null;
        startUrl = url;
        userAgent = agent;
        domains = domainList.isEmpty() ? new String[0] : domainList.split(",");
        required = requiredList.isEmpty() ? new String[0] : requiredList.split("\n");
        storage = storageList.isEmpty() ? new String[0] : storageList.split("\n");
        script = extractJs;
        found.clear();
        try {
            done = pattern.isEmpty() ? null : Pattern.compile(pattern);
        } catch (PatternSyntaxException e) {
            // Go's regexp syntax is close to Java's but not the same; the user can still close the page.
            done = null;
        }
        activity.startActivity(new Intent(activity, WebLogin.class));
        try {
            wait.await();
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            return null;
        }
        return result;
    }

    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        web = new WebView(this);
        // Debug builds can be inspected from a computer (chrome://inspect), to see why a page fails.
        if ((getApplicationInfo().flags & ApplicationInfo.FLAG_DEBUGGABLE) != 0) {
            WebView.setWebContentsDebuggingEnabled(true);
        }
        WebSettings settings = web.getSettings();
        settings.setJavaScriptEnabled(true);
        settings.setDomStorageEnabled(true);
        settings.setDatabaseEnabled(true);
        // The bridge asks for its own, desktop, user agent, so sites send their desktop pages:
        // lay those out at desktop width and let the user zoom, or parts end up off screen.
        settings.setUseWideViewPort(true);
        settings.setLoadWithOverviewMode(true);
        settings.setSupportZoom(true);
        settings.setBuiltInZoomControls(true);
        settings.setDisplayZoomControls(false);
        // Captchas and identity checks open in new windows and embed other sites' frames.
        settings.setJavaScriptCanOpenWindowsAutomatically(true);
        settings.setSupportMultipleWindows(true);
        settings.setMixedContentMode(WebSettings.MIXED_CONTENT_COMPATIBILITY_MODE);
        settings.setMediaPlaybackRequiresUserGesture(false);
        if (userAgent != null && !userAgent.isEmpty()) {
            settings.setUserAgentString(userAgent);
        }
        web.setWebChromeClient(new WebChromeClient() {
            // A new window loads in this one: there is only one view to show it in.
            @Override
            public boolean onCreateWindow(WebView view, boolean isDialog, boolean isUserGesture, Message resultMsg) {
                WebView popup = new WebView(WebLogin.this);
                popup.setWebViewClient(new WebViewClient() {
                    @Override
                    public boolean shouldOverrideUrlLoading(WebView v, android.webkit.WebResourceRequest request) {
                        web.loadUrl(request.getUrl().toString());
                        v.destroy();
                        return true;
                    }
                });
                ((WebView.WebViewTransport) resultMsg.obj).setWebView(popup);
                resultMsg.sendToTarget();
                return true;
            }
        });
        CookieManager.getInstance().setAcceptCookie(true);
        CookieManager.getInstance().setAcceptThirdPartyCookies(web, true);
        web.setWebViewClient(new WebViewClient() {
            @Override
            public void onPageFinished(WebView view, String url) {
                start(view);
                check(url);
            }

            @Override
            public void doUpdateVisitedHistory(WebView view, String url, boolean isReload) {
                check(url);
            }

            @Override
            public boolean shouldOverrideUrlLoading(WebView view, android.webkit.WebResourceRequest request) {
                return arrivedBack(request.getUrl().toString());
            }
        });
        setContentView(web);
        web.loadUrl(startUrl);
        if (storage.length > 0 || !script.isEmpty()) {
            handler.postDelayed(probe, PROBE_MS);
        }
    }

    /** Runs the bridge's script once per page; its result is read back by {@link #probe}. */
    private static void start(WebView view) {
        if (script.isEmpty()) {
            return;
        }
        view.evaluateJavascript(
                "(function(){if(window.__unisendLogin!==undefined)return;window.__unisendLogin=null;"
                        + "Promise.resolve(" + script + ").then(function(r){window.__unisendLogin=JSON.stringify(r||{});},function(){});})()",
                null);
    }

    /** Reads local storage and the script's result, then checks whether the sign-in is done. */
    private void probe() {
        if (web == null) {
            return;
        }
        StringBuilder js = new StringBuilder("JSON.stringify({\"\":window.__unisendLogin||null");
        for (String key : storage) {
            js.append(',').append(JSONObject.quote("s:" + key)).append(":localStorage.getItem(").append(JSONObject.quote(key)).append(')');
        }
        web.evaluateJavascript(js.append("})").toString(), value -> {
            take(value);
            if (web != null) {
                check(web.getUrl());
                handler.postDelayed(probe, PROBE_MS);
            }
        });
    }

    /** `value` is the JSON text of a JSON string, as evaluateJavascript gives it. */
    private static void take(String value) {
        try {
            Object text = new JSONTokener(value).nextValue();
            if (!(text instanceof String)) {
                return;
            }
            JSONObject all = new JSONObject((String) text);
            for (java.util.Iterator<String> keys = all.keys(); keys.hasNext(); ) {
                String key = keys.next();
                String v = all.optString(key, "");
                if (key.isEmpty()) {
                    Object result = all.opt("");
                    if (result instanceof String) {
                        JSONObject fields = new JSONObject((String) result);
                        for (java.util.Iterator<String> ids = fields.keys(); ids.hasNext(); ) {
                            String id = ids.next();
                            String f = fields.optString(id, "");
                            if (!f.isEmpty() && !"null".equals(f)) {
                                found.put("f:" + id, f);
                            }
                        }
                    }
                } else if (!v.isEmpty() && !"null".equals(v) && !all.isNull(key)) {
                    found.put(key, v);
                }
            }
        } catch (JSONException e) {
            // A page that has not got there yet, or a result that is not an object.
        }
    }

    private void check(String url) {
        if (arrivedBack(url)) {
            return;
        }
        boolean arrived = done == null || (url != null && done.matcher(url).find());
        if (arrived && required.length > 0 && hasRequired()) {
            finish();
        }
    }

    /**
     * A Matrix server's sign-in asks for "u:url": the address the page sends the browser back to
     * is then the answer itself, kept as "url" and never loaded.
     */
    private boolean arrivedBack(String url) {
        if (url == null || done == null || !Arrays.asList(required).contains("u:url") || !done.matcher(url).find()) {
            return false;
        }
        found.put("u:url", url);
        finish();
        return true;
    }

    /** Every required field has a value from one of its sources. */
    private static boolean hasRequired() {
        for (String field : required) {
            boolean any = false;
            for (String source : field.split("\t")) {
                any |= has(source);
            }
            if (!any) {
                return false;
            }
        }
        return true;
    }

    private static boolean has(String source) {
        if (source.startsWith("c:")) {
            String cookie = source.substring(2);
            int at = cookie.indexOf('@');
            String name = at < 0 ? cookie : cookie.substring(0, at);
            String jar = at < 0 ? "" : cookies(cookie.substring(at + 1));
            return ("; " + jar).contains("; " + name + "=");
        }
        return found.containsKey(source);
    }

    /** The domain's cookies as "name=value; ...", whether set for the bare domain or for www. */
    private static String cookies(String domain) {
        CookieManager manager = CookieManager.getInstance();
        StringBuilder all = new StringBuilder();
        for (String host : new String[] {domain, "www." + domain}) {
            String jar = manager.getCookie("https://" + host);
            if (jar != null && !jar.isEmpty()) {
                if (all.length() > 0) {
                    all.append("; ");
                }
                all.append(jar);
            }
        }
        return all.toString();
    }

    @Override
    public void onBackPressed() {
        if (web != null && web.canGoBack()) {
            web.goBack();
        } else {
            super.onBackPressed();
        }
    }

    @Override
    protected void onDestroy() {
        handler.removeCallbacksAndMessages(null);
        CookieManager.getInstance().flush();
        JSONObject all = new JSONObject();
        try {
            for (String domain : domains) {
                for (String pair : cookies(domain).split("; ")) {
                    int eq = pair.indexOf('=');
                    if (eq > 0) {
                        all.put(pair.substring(0, eq), pair.substring(eq + 1));
                    }
                }
            }
            // Local storage by its key, the script's result by field ID.
            for (Map.Entry<String, String> e : found.entrySet()) {
                all.put(e.getKey().substring(2), e.getValue());
            }
        } catch (JSONException e) {
            // put() only throws for a null key or a non-finite number.
        }
        found.clear();
        result = all.length() > 0 ? all.toString() : null;
        // The bridge has its cookies now: leave no signed-in session of that network in the app.
        if (web != null) {
            web.clearCache(true);
            web.clearFormData();
            web.clearHistory();
            web.destroy();
        }
        // Removal is asynchronous: write the empty jar to disk once it is done.
        CookieManager.getInstance().removeAllCookies(removed -> CookieManager.getInstance().flush());
        WebStorage.getInstance().deleteAllData();
        CountDownLatch wait = latch;
        if (wait != null) {
            wait.countDown();
        }
        super.onDestroy();
    }
}
