module Wid
  module Nodes
    class WhileStatement < Node
      attr_reader :test, :body

      def initialize(test, body)
        @test = test
        @body = body
      end

      def children
        [@test, @body]
      end

      def to_hash
        {class: self.class, test: @test.to_hash, body: @body.to_hash}
      end
    end
  end
end
