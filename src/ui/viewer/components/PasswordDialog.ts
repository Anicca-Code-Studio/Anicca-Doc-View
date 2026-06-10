/**
 * Password dialog component for encrypted PDF documents.
 */

import type { Store } from "../../framework/store";
import type { ViewerState } from "../state";
import type { Action } from "../actions";
import type { I18n } from "../i18n/index.js";
import { trapFocus } from "../a11y";

export interface PasswordDialogCallbacks {
    onSubmit: (password: string) => void;
}

export function createPasswordDialog() {
    // Create overlay
    const overlay = document.createElement("div");
    overlay.className = "adv-password-overlay";

    // Create dialog
    const dialog = document.createElement("div");
    dialog.className = "adv-password-dialog";
    dialog.setAttribute("role", "dialog");
    dialog.setAttribute("aria-labelledby", "adv-password-title");
    dialog.setAttribute("aria-modal", "true");

    // Dialog content
    dialog.innerHTML = `
        <div class="adv-password-header">
            <svg class="adv-password-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                <rect x="3" y="11" width="18" height="11" rx="2" ry="2"/>
                <path d="M7 11V7a5 5 0 0 1 10 0v4"/>
            </svg>
            <h2 id="adv-password-title" class="adv-password-title">Password Required</h2>
        </div>
        <p class="adv-password-message">This document is protected. Please enter the password to open it.</p>
        <form class="adv-password-form">
            <div class="adv-password-input-wrapper">
                <input
                    type="password"
                    class="adv-password-input"
                    placeholder="Enter password"
                    autocomplete="off"
                    aria-label="Password"
                    aria-describedby="adv-password-error"
                />
                <button type="button" class="adv-password-toggle" aria-label="Show password">
                    <svg class="adv-password-eye-open" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                        <path d="M1 12s4-8 11-8 11 8 11 8-4 8-11 8-11-8-11-8z"/>
                        <circle cx="12" cy="12" r="3"/>
                    </svg>
                    <svg class="adv-password-eye-closed" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" style="display:none">
                        <path d="M17.94 17.94A10.07 10.07 0 0 1 12 20c-7 0-11-8-11-8a18.45 18.45 0 0 1 5.06-5.94M9.9 4.24A9.12 9.12 0 0 1 12 4c7 0 11 8 11 8a18.5 18.5 0 0 1-2.16 3.19m-6.72-1.07a3 3 0 1 1-4.24-4.24"/>
                        <line x1="1" y1="1" x2="23" y2="23"/>
                    </svg>
                </button>
            </div>
            <p class="adv-password-error" id="adv-password-error" aria-live="polite"></p>
            <button type="submit" class="adv-password-submit">
                <span class="adv-password-submit-text">Unlock</span>
                <span class="adv-password-submit-spinner" style="display:none">
                    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                        <circle cx="12" cy="12" r="10" stroke-dasharray="31.4 31.4" stroke-dashoffset="0">
                            <animateTransform attributeName="transform" type="rotate" from="0 12 12" to="360 12 12" dur="1s" repeatCount="indefinite"/>
                        </circle>
                    </svg>
                </span>
            </button>
        </form>
    `;

    overlay.appendChild(dialog);

    // Get elements
    const form = dialog.querySelector(".adv-password-form") as HTMLFormElement;
    const input = dialog.querySelector(".adv-password-input") as HTMLInputElement;
    const toggleBtn = dialog.querySelector(".adv-password-toggle") as HTMLButtonElement;
    const eyeOpen = dialog.querySelector(".adv-password-eye-open") as SVGElement;
    const eyeClosed = dialog.querySelector(".adv-password-eye-closed") as SVGElement;
    const errorEl = dialog.querySelector(".adv-password-error") as HTMLParagraphElement;
    const submitBtn = dialog.querySelector(".adv-password-submit") as HTMLButtonElement;
    const submitText = dialog.querySelector(".adv-password-submit-text") as HTMLSpanElement;
    const submitSpinner = dialog.querySelector(".adv-password-submit-spinner") as HTMLSpanElement;

    let callbacks: PasswordDialogCallbacks | null = null;
    let unsubRender: (() => void) | null = null;
    let cleanupTrap: (() => void) | null = null;
    let previousFocus: HTMLElement | null = null;

    // Handle form submit
    form.addEventListener("submit", (e) => {
        e.preventDefault();
        const password = input.value;
        if (password && callbacks?.onSubmit) {
            callbacks.onSubmit(password);
        }
    });

    // Clear error when typing
    input.addEventListener("input", () => {
        if (errorEl.textContent) {
            errorEl.textContent = "";
            errorEl.style.display = "none";
        }
    });

    function mount(
        container: HTMLElement,
        store: Store<ViewerState, Action>,
        i18n: I18n,
        cb: PasswordDialogCallbacks,
    ): void {
        container.appendChild(overlay);
        callbacks = cb;

        // Update i18n strings in the dialog
        const titleEl = dialog.querySelector("#adv-password-title") as HTMLElement;
        if (titleEl) titleEl.textContent = i18n.t("password.title");
        const messageEl = dialog.querySelector(".adv-password-message") as HTMLElement;
        if (messageEl) messageEl.textContent = i18n.t("password.message");
        input.placeholder = i18n.t("password.placeholder");
        input.setAttribute("aria-label", i18n.t("password.label"));
        toggleBtn.setAttribute("aria-label", i18n.t("password.showPassword"));
        const submitTextEl = dialog.querySelector(".adv-password-submit-text") as HTMLElement;
        if (submitTextEl) submitTextEl.textContent = i18n.t("password.unlock");

        // Toggle password visibility
        toggleBtn.addEventListener("click", () => {
            const isPassword = input.type === "password";
            input.type = isPassword ? "text" : "password";
            eyeOpen.style.display = isPassword ? "none" : "";
            eyeClosed.style.display = isPassword ? "" : "none";
            toggleBtn.setAttribute(
                "aria-label",
                isPassword ? i18n.t("password.hidePassword") : i18n.t("password.showPassword"),
            );
        });

        unsubRender = store.subscribeRender((prev, next) => {
            // Show/hide dialog based on needsPassword state
            const wasVisible = prev.needsPassword && prev.doc !== null;
            const isVisible = next.needsPassword && next.doc !== null;

            if (wasVisible !== isVisible) {
                overlay.style.display = isVisible ? "" : "none";
                if (isVisible) {
                    // Save focus to restore on close
                    previousFocus = document.activeElement as HTMLElement | null;
                    // Focus input when dialog appears
                    setTimeout(() => input.focus(), 0);
                    // Reset input
                    input.value = "";
                    input.type = "password";
                    eyeOpen.style.display = "";
                    eyeClosed.style.display = "none";
                    // Trap focus inside dialog
                    cleanupTrap = trapFocus(dialog);
                } else {
                    if (cleanupTrap) {
                        cleanupTrap();
                        cleanupTrap = null;
                    }
                    if (previousFocus && previousFocus.focus) {
                        previousFocus.focus();
                        previousFocus = null;
                    }
                }
            }

            // Update error message
            if (prev.passwordError !== next.passwordError) {
                errorEl.textContent = next.passwordError ?? "";
                errorEl.style.display = next.passwordError ? "" : "none";
            }

            // Update loading state
            if (prev.isAuthenticating !== next.isAuthenticating) {
                submitBtn.disabled = next.isAuthenticating;
                input.disabled = next.isAuthenticating;
                submitText.style.display = next.isAuthenticating ? "none" : "";
                submitSpinner.style.display = next.isAuthenticating ? "" : "none";
            }
        });

        // Check initial state
        const initialState = store.getState();
        const isVisible = initialState.needsPassword && initialState.doc !== null;
        overlay.style.display = isVisible ? "" : "none";
        errorEl.style.display = initialState.passwordError ? "" : "none";
        errorEl.textContent = initialState.passwordError ?? "";

        if (isVisible) {
            setTimeout(() => input.focus(), 0);
        }
    }

    function destroy(): void {
        if (unsubRender) unsubRender();
        if (cleanupTrap) {
            cleanupTrap();
            cleanupTrap = null;
        }
        callbacks = null;
        previousFocus = null;
        overlay.remove();
    }

    return { el: overlay, mount, destroy };
}
