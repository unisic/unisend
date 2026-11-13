package dev.gpui.beeper;

import android.app.Activity;
import android.content.Intent;
import android.content.pm.ApplicationInfo;
import android.os.Bundle;
import android.os.Message;
import android.webkit.CookieManager;
import android.webkit.WebChromeClient;
import android.webkit.WebSettings;
import android.webkit.WebView;
import android.webkit.WebViewClient;
import java.util.concurrent.CountDownLatch;
import java.util.regex.Pattern;
import java.util.regex.PatternSyntaxException;

/**
 * A browser for signing in to a chat network. It closes once the address matches the pattern the
 * bridge gave and the cookies the bridge needs exist, and hands back the site's cookies, which is
 * what the bridge logs in with. The pattern alone is not enough: Messenger's also matches its own
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
    /** "name@domain" for each cookie the bridge cannot do without. */
    private static String[] required;

    private WebView web;

    /** Returns "name=value; ..." for the given domains, or null when there are no cookies. */
    public static String run(
            Activity activity, String url, String agent, String pattern, String domainList, String requiredList) {
        CountDownLatch wait = new CountDownLatch(1);
        latch = wait;
        result = null;
        startUrl = url;
        userAgent = agent;
        domains = domainList.isEmpty() ? new String[0] : domainList.split(",");
        required = requiredList.isEmpty() ? new String[0] : requiredList.split(",");
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
                check(url);
            }

            @Override
            public void doUpdateVisitedHistory(WebView view, String url, boolean isReload) {
                check(url);
            }
        });
        setContentView(web);
        web.loadUrl(startUrl);
    }

    private void check(String url) {
        boolean arrived = done == null || (url != null && done.matcher(url).find());
        if (arrived && required.length > 0 && hasRequiredCookies()) {
            finish();
        }
    }

    private static boolean hasRequiredCookies() {
        for (String cookie : required) {
            int at = cookie.indexOf('@');
            String name = at < 0 ? cookie : cookie.substring(0, at);
            String jar = at < 0 ? "" : cookies(cookie.substring(at + 1));
            if (!("; " + jar).contains("; " + name + "=")) {
                return false;
            }
        }
        return true;
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
        CookieManager.getInstance().flush();
        StringBuilder all = new StringBuilder();
        for (String domain : domains) {
            String jar = cookies(domain);
            if (!jar.isEmpty()) {
                if (all.length() > 0) {
                    all.append("; ");
                }
                all.append(jar);
            }
        }
        result = all.length() > 0 ? all.toString() : null;
        if (web != null) {
            web.destroy();
        }
        CountDownLatch wait = latch;
        if (wait != null) {
            wait.countDown();
        }
        super.onDestroy();
    }
}
