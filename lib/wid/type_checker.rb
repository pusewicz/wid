      return false unless input.is_a?(String)
      return input.end_with?('"') if input.start_with?('"')
      return input.end_with?("'") if input.start_with?("'")

      false
    end
  end
end
