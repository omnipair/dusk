impl RiskRateBook {
    /// The operator handler authenticates the timelocked bundle before calling this.
    /// Accrue under the old model, then install the new parameters prospectively.
    /// Changing the creation default never resets an existing market's APR.
    pub fn replace_config(
        &mut self,
        config: RiskRateConfig,
        inputs: [RiskRateInput; 2],
        current_slot: u64,
    ) -> Result<[RateAccrual; 2]> {
        config.validate()?;
        require_eq!(
            config.initial_apr_nad,
            self.config.initial_apr_nad,
            ErrorCode::OperatorFieldForbidden
        );
        let results = self.accrue(inputs, current_slot)?;
        self.config = config;
        Ok(results)
    }
}
