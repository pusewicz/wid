# frozen_string_literal: true

require_relative "spec_helper"

describe Wid::Tokenizer do
  def tokenize(string)
    tokenizer = ::Wid::Tokenizer.new(string)
    tokens = []
    while (token = tokenizer.next_token)
      tokens << [token.type, token.value]
    end

    tokens
  end

  it "tokenizes integers" do
    %w[3 25].each do |number|
      assert_equal([[:INT, number]], tokenize(number))
    end
  end

  it "tokenizes floats" do
    %w[3.1 25.8].each do |number|
      assert_equal([[:FLOAT, number]], tokenize(number))
    end
  end
end
